//! SVG and Chromium vector icons use different paint models. Resolve SVG once
//! with usvg, then emit explicit geometry and paint operations. Unsupported
//! compositing requires an explicit approximation request.
use std::fmt::Write;
use std::path::Path;
use tiny_skia::{Path as VectorPath, PathSegment, Rect, Transform};

#[path = "chromium_icon_effects.rs"]
mod effects;
#[path = "chromium_icon_reverse.rs"]
mod reverse;

fn format_number(value: f32) -> String {
    if value == 0.0 {
        "0".into()
    } else {
        value.to_string()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SvgToChromiumIconOptions {
    pub emit_path_colors: bool,
    pub approximate_effects: bool,
}

impl Default for SvgToChromiumIconOptions {
    fn default() -> Self {
        Self {
            emit_path_colors: true,
            approximate_effects: false,
        }
    }
}

struct Layer {
    path: VectorPath,
    color: usvg::Color,
    alpha: f32,
    rule: usvg::FillRule,
    clip: Rect,
    antialias: bool,
}

enum ConversionError {
    Effect(&'static str),
    Invalid(String),
}

type ConversionResult<T> = Result<T, ConversionError>;

fn solid_color(paint: &usvg::Paint) -> ConversionResult<usvg::Color> {
    match paint {
        usvg::Paint::Color(color) => Ok(*color),
        _ => Err(ConversionError::Effect("渐变或图案填充")),
    }
}

// Chromium CLIP is one global axis-aligned rectangle, not a path-local state.
fn clip_rectangle(transform: Transform, clip: &usvg::ClipPath) -> ConversionResult<Rect> {
    if clip.clip_path().is_some() || clip.root().children().len() != 1 {
        return Err(ConversionError::Effect("复合裁剪路径"));
    }
    let usvg::Node::Path(path) = &clip.root().children()[0] else {
        return Err(ConversionError::Effect("复合裁剪路径"));
    };
    if !path.is_visible() || path.fill().is_none() {
        return Err(ConversionError::Effect("空裁剪路径"));
    }
    let transform = transform
        .pre_concat(clip.transform())
        .pre_concat(clip.root().transform());
    let path = path
        .data()
        .clone()
        .transform(transform)
        .ok_or_else(|| ConversionError::Invalid("裁剪坐标超出有效范围".into()))?;
    let mut points = Vec::new();
    let mut closed = false;
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(p) if points.is_empty() => points.push(p),
            PathSegment::LineTo(p) if !closed => points.push(p),
            PathSegment::Close if !closed => closed = true,
            _ => return Err(ConversionError::Effect("非矩形裁剪路径")),
        }
    }
    if points.len() == 5 && points.first() == points.last() {
        points.pop();
    }
    if !closed || points.len() != 4 {
        return Err(ConversionError::Effect("非矩形裁剪路径"));
    }
    // Four axis-aligned edges alone also admit a retraced, zero-area L shape.
    if (0..4).any(|i| (i + 1..4).any(|j| points[i] == points[j])) {
        return Err(ConversionError::Effect("非矩形裁剪路径"));
    }
    let bounds = path.bounds();
    for i in 0..4 {
        let a = points[i];
        let b = points[(i + 1) % 4];
        if !((a.x == bounds.left() || a.x == bounds.right())
            && (a.y == bounds.top() || a.y == bounds.bottom())
            && ((a.x == b.x) ^ (a.y == b.y)))
        {
            return Err(ConversionError::Effect("非矩形裁剪路径"));
        }
    }
    Ok(bounds)
}

fn transformed(path: VectorPath, transform: Transform) -> ConversionResult<VectorPath> {
    path.transform(transform)
        .ok_or_else(|| ConversionError::Invalid("路径变换产生了无效坐标".into()))
}

fn collect_path(
    path: &usvg::Path,
    transform: Transform,
    clip: Rect,
    layers: &mut Vec<Layer>,
) -> ConversionResult<()> {
    if !path.is_visible() {
        return Ok(());
    }
    let mut fill_layer = None;
    if let Some(fill) = path.fill().filter(|fill| fill.opacity().get() > 0.0) {
        fill_layer = Some(Layer {
            path: transformed(path.data().clone(), transform)?,
            color: solid_color(fill.paint())?,
            alpha: fill.opacity().get(),
            rule: fill.rule(),
            clip,
            antialias: path.rendering_mode().use_shape_antialiasing(),
        });
    }
    let mut stroke_layer = None;
    if let Some(stroke) = path.stroke().filter(|stroke| stroke.opacity().get() > 0.0) {
        let color = solid_color(stroke.paint())?;
        let mut style = stroke.to_tiny_skia();
        let resolution_scale =
            tiny_skia::PathStroker::compute_resolution_scale(&transform).max(1.0) * 8.0;
        // Expand before applying the full affine transform, preserving SVG caps,
        // joins, dashes, and non-uniformly scaled stroke outlines.
        let geometry = if let Some(dash) = style.dash.take() {
            path.data().dash(&dash, resolution_scale)
        } else {
            Some(path.data().clone())
        };
        if let Some(outline) =
            geometry.and_then(|geometry| geometry.stroke(&style, resolution_scale))
        {
            stroke_layer = Some(Layer {
                path: transformed(outline, transform)?,
                color,
                alpha: stroke.opacity().get(),
                rule: usvg::FillRule::NonZero,
                clip,
                antialias: path.rendering_mode().use_shape_antialiasing(),
            });
        }
    }
    match path.paint_order() {
        usvg::PaintOrder::FillAndStroke => {
            layers.extend(fill_layer.into_iter().chain(stroke_layer))
        }
        usvg::PaintOrder::StrokeAndFill => {
            layers.extend(stroke_layer.into_iter().chain(fill_layer))
        }
    }
    Ok(())
}

fn collect_layers(
    group: &usvg::Group,
    parent_transform: Transform,
    inherited_clip: Rect,
    layers: &mut Vec<Layer>,
) -> ConversionResult<()> {
    // Accumulate the instance's group transforms, as resvg does. usvg's cached
    // absolute transforms can be stale for cloned <use> subtrees.
    let transform = parent_transform.pre_concat(group.transform());
    if group.opacity().get() == 0.0 {
        return Ok(());
    }
    if group.mask().is_some() {
        return Err(ConversionError::Effect("蒙版"));
    }
    if !group.filters().is_empty() {
        return Err(ConversionError::Effect("滤镜"));
    }
    if group.blend_mode() != usvg::BlendMode::Normal {
        return Err(ConversionError::Effect("混合模式"));
    }
    let clip = if let Some(clip) = group.clip_path() {
        let Some(rect) = inherited_clip.intersect(&clip_rectangle(transform, clip)?) else {
            return Ok(());
        };
        rect
    } else {
        inherited_clip
    };
    let start = layers.len();
    for child in group.children() {
        match child {
            usvg::Node::Group(child) => collect_layers(child, transform, clip, layers)?,
            usvg::Node::Path(path) => collect_path(path, transform, clip, layers)?,
            usvg::Node::Image(_) => return Err(ConversionError::Effect("嵌入式位图")),
            usvg::Node::Text(_) => {
                return Err(ConversionError::Invalid("请先将文字转为路径".into()))
            }
        }
    }
    if group.opacity().get() < 1.0 {
        if layers.len() - start > 1 {
            return Err(ConversionError::Effect("多图层组透明度"));
        }
        for layer in &mut layers[start..] {
            layer.alpha *= group.opacity().get();
        }
    }
    Ok(())
}

fn emit_path(output: &mut String, path: &VectorPath, dx: f32, dy: f32) -> ConversionResult<()> {
    for segment in path.segments() {
        let (command, points) = match segment {
            PathSegment::MoveTo(p) => ("MOVE_TO", vec![p]),
            PathSegment::LineTo(p) => ("LINE_TO", vec![p]),
            PathSegment::QuadTo(a, b) => ("QUADRATIC_TO", vec![a, b]),
            PathSegment::CubicTo(a, b, c) => ("CUBIC_TO", vec![a, b, c]),
            PathSegment::Close => {
                output.push_str("CLOSE,\n");
                continue;
            }
        };
        output.push_str(command);
        for point in points {
            let (x, y) = (point.x + dx, point.y + dy);
            if !x.is_finite() || !y.is_finite() {
                return Err(ConversionError::Invalid("输出路径包含非有限坐标".into()));
            }
            write!(output, ", {}, {}", format_number(x), format_number(y)).unwrap();
        }
        output.push_str(",\n");
    }
    Ok(())
}

fn emit_tree(tree: &usvg::Tree, options: &SvgToChromiumIconOptions) -> ConversionResult<String> {
    let (width, height) = (tree.size().width(), tree.size().height());
    let canvas = width.max(height).ceil().max(1.0);
    if canvas > 16384.0 {
        return Err(ConversionError::Invalid("图标画布不能超过 16384".into()));
    }
    let viewport = Rect::from_xywh(0.0, 0.0, width, height).unwrap();
    let mut layers = Vec::new();
    collect_layers(tree.root(), Transform::identity(), viewport, &mut layers)?;
    let mut global_clip = viewport;
    for layer in &layers {
        global_clip = global_clip
            .intersect(&layer.clip)
            .ok_or(ConversionError::Effect("相互独立的裁剪区域"))?;
    }
    for layer in &layers {
        if layer.clip != global_clip {
            if let Some(visible_bounds) = layer.path.bounds().intersect(&layer.clip) {
                if visible_bounds.intersect(&global_clip) != Some(visible_bounds) {
                    return Err(ConversionError::Effect("相互独立的裁剪区域"));
                }
            }
        }
    }
    let (dx, dy) = ((canvas - width) / 2.0, (canvas - height) / 2.0);
    let mut output = format!(
        "CANVAS_DIMENSIONS, {},\nCLIP, {}, {}, {}, {},\n",
        format_number(canvas),
        format_number(global_clip.left() + dx),
        format_number(global_clip.top() + dy),
        format_number(global_clip.width()),
        format_number(global_clip.height())
    );
    for (index, layer) in layers.iter().enumerate() {
        if index > 0 {
            output.push_str("NEW_PATH,\n");
        }
        let alpha = (layer.alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        if options.emit_path_colors {
            writeln!(
                output,
                "PATH_COLOR_ARGB, 0x{alpha:02X}, 0x{:02X}, 0x{:02X}, 0x{:02X},",
                layer.color.red, layer.color.green, layer.color.blue
            )
            .unwrap();
        } else {
            writeln!(output, "PATH_COLOR_ALPHA, {alpha},").unwrap();
        }
        if layer.rule == usvg::FillRule::NonZero {
            output.push_str("FILL_RULE_NONZERO,\n");
        }
        if !layer.antialias {
            output.push_str("DISABLE_AA,\n");
        }
        emit_path(&mut output, &layer.path, dx, dy)?;
    }
    if layers.is_empty() {
        output.push_str("PATH_COLOR_ALPHA, 0,\nMOVE_TO, 0, 0,\nCLOSE,\n");
    }
    Ok(output)
}

pub fn try_convert_svg_source_to_chromium_icon(
    source: &str,
    options: &SvgToChromiumIconOptions,
) -> Result<String, String> {
    let tree = super::svg_document::parse_svg(source)?;
    match emit_tree(&tree, options) {
        Ok(icon) => Ok(icon),
        Err(ConversionError::Invalid(error)) => Err(error),
        Err(ConversionError::Effect(effect)) if options.approximate_effects => {
            let icon = effects::flatten_tree(&tree, options.emit_path_colors)?;
            Ok(format!("// Approximation requested: {effect}\n{icon}"))
        }
        Err(ConversionError::Effect(effect)) => Err(format!(
            "SVG 包含无法等价表示为 Chromium .icon 的{effect}；请简化为普通路径，或显式启用“近似复杂效果”")),
    }
}

pub fn try_convert_svg_to_chromium_icon_with_options(
    svg_path: &str,
    output_path: &str,
    options: &SvgToChromiumIconOptions,
) -> Result<String, String> {
    let source = Path::new(svg_path);
    let target = source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(output_path);
    if source == target
        || (target.exists()
            && std::fs::canonicalize(source).ok() == std::fs::canonicalize(&target).ok())
    {
        return Err("转换输出不能覆盖源 SVG".into());
    }
    let content = std::fs::read_to_string(source).map_err(|e| format!("读取 SVG 失败: {e}"))?;
    let icon = try_convert_svg_source_to_chromium_icon(&content, options)?;
    super::image_util::write_output_atomic(&target, icon.as_bytes())?;
    Ok(target.to_string_lossy().into_owned())
}

pub fn try_convert_svg_to_chromium_icon(
    svg_path: &str,
    output_path: &str,
) -> Result<String, String> {
    try_convert_svg_to_chromium_icon_with_options(
        svg_path,
        output_path,
        &SvgToChromiumIconOptions::default(),
    )
}

pub fn convert_svg_to_chromium_icon(svg_path: &str, output_path: &str) -> String {
    try_convert_svg_to_chromium_icon(svg_path, output_path)
        .expect("Failed to convert SVG to Chromium icon")
}

pub fn try_convert_chromium_icon_source_to_svg_markup(source: &str) -> Result<String, String> {
    reverse::convert(source, None)
}

pub fn try_convert_chromium_icon_path_to_svg_markup(icon_path: &str) -> Result<String, String> {
    let source = std::fs::read_to_string(icon_path).map_err(|e| format!("读取 .icon 失败: {e}"))?;
    try_convert_chromium_icon_source_to_svg_markup(&source)
}

pub fn try_convert_chromium_icon_path_to_svg_markup_with_backdrop(
    icon_path: &str,
    backdrop: Option<&str>,
) -> Result<String, String> {
    let source = std::fs::read_to_string(icon_path).map_err(|e| format!("读取 .icon 失败: {e}"))?;
    let fill = match backdrop.map(str::trim) {
        Some("dark" | "black") => "#111111",
        _ => "#ffffff",
    };
    reverse::convert(&source, Some(fill))
}

#[cfg(test)]
#[path = "chromium_icon_tests.rs"]
mod tests;
