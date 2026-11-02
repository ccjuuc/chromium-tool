//! Bounded visual fallback for SVG effects that Chromium's command format cannot encode.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::ops::Range;

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg;
use svg::node::Value;

const SAMPLE_SIZE: u32 = 128;
const COLOR_STEP: u16 = 8;
const TEMPLATE_ALPHA_THRESHOLD: u8 = 64;

type SampleColor = [u8; 4]; // RGBA, with unpremultiplied RGB.

#[derive(Clone, Copy)]
struct SampleRect {
    color: SampleColor,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct ExporterCanvas {
    range: Range<usize>,
    radius: f32,
}

/// Rasterize compositing first, then encode merged, equally colored samples as
/// ordinary Chromium paths. The result has no bitmap dependency and always has
/// at most 128 * 128 rectangles, regardless of the SVG's original dimensions.
pub(super) fn flatten_svg(svg: &str, emit_path_colors: bool) -> Result<String, String> {
    let canvas = exporter_canvas(svg)?;
    let prepared = if emit_path_colors {
        svg.to_string()
    } else {
        remove_canvas(svg, canvas.as_ref())
    };
    let tree = usvg::Tree::from_str(&prepared, &usvg::Options::default())
        .map_err(|error| format!("SVG 效果展开失败: {}", error))?;
    let mut pixmap = Pixmap::new(SAMPLE_SIZE, SAMPLE_SIZE)
        .ok_or_else(|| "无法创建 SVG 效果预览画布".to_string())?;
    let scale = SAMPLE_SIZE as f32 / tree.size().width().max(tree.size().height());
    let dx = (SAMPLE_SIZE as f32 - tree.size().width() * scale) / 2.0;
    let dy = (SAMPLE_SIZE as f32 - tree.size().height() * scale) / 2.0;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale).post_translate(dx, dy),
        &mut pixmap.as_mut(),
    );

    // Keep an exact opaque backing for a simple exporter canvas. Otherwise AA
    // between disjoint sampled colors can reveal the preview background.
    let backing = if emit_path_colors {
        canvas.as_ref().map(render_canvas).transpose()?
    } else {
        None
    };
    let colors: Vec<SampleColor> = pixmap
        .pixels()
        .iter()
        .enumerate()
        .map(|(index, pixel)| {
            let color = pixel.demultiply();
            if emit_path_colors {
                let mut sample = [
                    quantize(color.red()),
                    quantize(color.green()),
                    quantize(color.blue()),
                    quantize(color.alpha()),
                ];
                if sample[..3] == [255, 255, 255]
                    && backing.as_ref().is_some_and(|background| {
                        sample[3] <= quantize(background.pixels()[index].alpha())
                    })
                {
                    sample[3] = 0;
                }
                sample
            } else if color.alpha() >= TEMPLATE_ALPHA_THRESHOLD {
                [0, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            }
        })
        .collect();
    let rectangles = merge_sample_runs(&colors, SAMPLE_SIZE, SAMPLE_SIZE);
    let mut groups: BTreeMap<SampleColor, Vec<SampleRect>> = BTreeMap::new();
    for rectangle in rectangles {
        groups.entry(rectangle.color).or_default().push(rectangle);
    }

    let mut output = format!(
        "// SVG effects approximated as sampled vector paths.\n\
         // Sampling: {0}x{0}; color channel step: {1}.\n\
         // Masks, filters and gradients are baked into the path colors.\n\
         CANVAS_DIMENSIONS, {0},\n",
        SAMPLE_SIZE, COLOR_STEP
    );
    if !emit_path_colors {
        writeln!(
            output,
            "// Template silhouette: samples with alpha >= {} are retained.",
            TEMPLATE_ALPHA_THRESHOLD
        )
        .unwrap();
    }
    if let Some(canvas) = canvas.as_ref().filter(|_| emit_path_colors) {
        writeln!(
            output,
            "PATH_COLOR_ARGB, 0xFF, 0xFF, 0xFF, 0xFF,\nROUND_RECT, 0, 0, {}, {}, {},",
            SAMPLE_SIZE,
            SAMPLE_SIZE,
            super::format_number(canvas.radius)
        )
        .unwrap();
    }
    if groups.is_empty() && backing.is_none() {
        // A valid mask may intentionally hide every pixel. Keep a valid invisible
        // representation instead of treating the absence of paint as a failure.
        output.push_str("MOVE_TO, 0, 0,\nCLOSE,\n");
        return Ok(output);
    }

    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|(color, _)| {
        let luminance =
            2126 * i32::from(color[0]) + 7152 * i32::from(color[1]) + 722 * i32::from(color[2]);
        (-luminance, *color)
    });
    for (index, (color, rectangles)) in groups.into_iter().enumerate() {
        if index > 0 || backing.is_some() {
            output.push_str("NEW_PATH,\n");
        }
        output.push_str("FILL_RULE_NONZERO,\n");
        if emit_path_colors {
            writeln!(
                output,
                "PATH_COLOR_ARGB, 0x{:02X}, 0x{:02X}, 0x{:02X}, 0x{:02X},",
                color[3], color[0], color[1], color[2]
            )
            .unwrap();
        }
        // One compound path per color avoids separate draw calls and internal
        // antialias seams between adjacent samples of the same color.
        for rectangle in rectangles {
            writeln!(
                output,
                "MOVE_TO, {}, {},\nR_H_LINE_TO, {},\nR_V_LINE_TO, {},\nR_H_LINE_TO, -{},\nCLOSE,",
                rectangle.x, rectangle.y, rectangle.width, rectangle.height, rectangle.width
            )
            .unwrap();
        }
    }
    Ok(output)
}

fn quantize(channel: u8) -> u8 {
    (((u16::from(channel) + COLOR_STEP / 2) / COLOR_STEP) * COLOR_STEP).min(255) as u8
}

fn merge_sample_runs(colors: &[SampleColor], width: u32, height: u32) -> Vec<SampleRect> {
    let mut rectangles: Vec<SampleRect> = Vec::new();
    let mut previous: HashMap<(u32, u32, SampleColor), usize> = HashMap::new();
    for y in 0..height {
        let mut current = HashMap::new();
        let mut x = 0;
        while x < width {
            let color = colors[(y * width + x) as usize];
            let mut end = x + 1;
            while end < width && colors[(y * width + end) as usize] == color {
                end += 1;
            }
            if color[3] != 0 {
                let key = (x, end, color);
                let index = if let Some(&index) = previous.get(&key) {
                    rectangles[index].height += 1;
                    index
                } else {
                    let index = rectangles.len();
                    rectangles.push(SampleRect {
                        color,
                        x,
                        y,
                        width: end - x,
                        height: 1,
                    });
                    index
                };
                current.insert(key, index);
            }
            x = end;
        }
        previous = current;
    }
    rectangles
}

fn render_canvas(canvas: &ExporterCanvas) -> Result<Pixmap, String> {
    let markup = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{0}" height="{0}"><rect width="{0}" height="{0}" rx="{1}" fill="white"/></svg>"#,
        SAMPLE_SIZE, canvas.radius
    );
    let tree = usvg::Tree::from_str(&markup, &usvg::Options::default())
        .map_err(|error| format!("SVG 背景展开失败: {}", error))?;
    let mut pixmap =
        Pixmap::new(SAMPLE_SIZE, SAMPLE_SIZE).ok_or_else(|| "无法创建 SVG 背景画布".to_string())?;
    resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());
    Ok(pixmap)
}

fn remove_canvas(svg: &str, canvas: Option<&ExporterCanvas>) -> String {
    let Some(canvas) = canvas else {
        return svg.to_string();
    };
    let mut prepared = String::with_capacity(svg.len() - canvas.range.len());
    prepared.push_str(&svg[..canvas.range.start]);
    prepared.push_str(&svg[canvas.range.end..]);
    prepared
}

/// Only the first visible, direct child can be an exporter canvas. Its square,
/// untransformed viewport and circular corners must map exactly to ROUND_RECT.
fn exporter_canvas(svg: &str) -> Result<Option<ExporterCanvas>, String> {
    type Attributes = HashMap<String, Value>;
    let document = usvg::roxmltree::Document::parse(svg)
        .map_err(|error| format!("SVG 效果展开失败: {}", error))?;
    let events = svg::read(svg)
        .map_err(|error| format!("SVG 效果展开失败: {}", error))?
        .collect::<Vec<_>>();
    let stylesheet = super::collect_svg_stylesheet(&events);
    if stylesheet.keys().any(|selector| {
        let name = selector.trim_start_matches(['.', '#']);
        name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    }) {
        // Complex selectors cannot be resolved by the shared lightweight CSS
        // parser, so retain the rendered canvas rather than guessing its paint.
        return Ok(None);
    }
    let resolve = |node: usvg::roxmltree::Node<'_, '_>, inherited: &Attributes| {
        let mut local: Attributes = node
            .attributes()
            .map(|attr| (attr.name().to_string(), Value::from(attr.value())))
            .collect();
        // Preserve non-inherited effects too: the shared paint resolver only
        // copies presentation attributes, but a CSS transform forbids this optimization.
        let mut selectors = vec![node.tag_name().name().to_string()];
        if let Some(classes) = node.attribute("class") {
            selectors.extend(
                classes
                    .split_whitespace()
                    .map(|class| format!(".{}", class)),
            );
        }
        if let Some(id) = node.attribute("id") {
            selectors.push(format!("#{}", id));
        }
        for selector in selectors {
            if let Some(declarations) = stylesheet.get(&selector) {
                local.extend(
                    declarations
                        .iter()
                        .map(|(key, value)| (key.clone(), Value::from(value.as_str()))),
                );
            }
        }
        if let Some(style) = node.attribute("style") {
            local.extend(
                super::parse_inline_style_decls(style)
                    .into_iter()
                    .map(|(key, value)| (key, Value::from(value))),
            );
        }
        super::resolve_svg_styles_with_inheritance(
            &HashMap::new(),
            &HashMap::new(),
            inherited,
            &local,
            node.tag_name().name(),
        )
    };
    let no_effects = |attributes: &Attributes| {
        [
            "transform",
            "mask",
            "mask-image",
            "filter",
            "clip-path",
            "clip",
            "backdrop-filter",
        ]
        .iter()
        .all(|key| {
            attributes
                .get(*key)
                .is_none_or(|value| value.to_string().trim() == "none")
        }) && attributes
            .get("display")
            .is_none_or(|value| value.to_string().trim() == "inline")
            && attributes
                .get("visibility")
                .is_none_or(|value| value.to_string().trim() == "visible")
            && attributes
                .get("mix-blend-mode")
                .is_none_or(|value| value.to_string().trim() == "normal")
            && super::effective_svg_paint_opacity(attributes, false) == 1.0
            && ["opacity", "fill-opacity"].iter().all(|key| {
                attributes
                    .get(*key)
                    .is_none_or(|value| value.to_string().trim().parse::<f32>() == Ok(1.0))
            })
    };
    let root = document.root_element();
    let root_style = resolve(root, &Attributes::new());
    if !no_effects(&root_style) {
        return Ok(None);
    }
    let plain_dimension = |value: &Value| {
        let text = value.to_string();
        let text = text.trim();
        text.strip_suffix("px")
            .unwrap_or(text)
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
    };
    let dimension = |key: &str| root_style.get(key).and_then(&plain_dimension);
    let viewport = match (dimension("width"), dimension("height")) {
        (Some(width), Some(height)) if width > 0.0 && (width - height).abs() < 0.01 => Some(width),
        (None, None) if !root_style.contains_key("width") && !root_style.contains_key("height") => {
            None
        }
        _ => return Ok(None),
    };
    let canvas_width = if let Some(view_box) = root_style.get("viewBox") {
        let parsed = view_box
            .to_string()
            .split(|ch: char| ch.is_whitespace() || ch == ',')
            .filter(|part| !part.is_empty())
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>();
        match parsed {
            Ok(values)
                if values.len() == 4
                    && values[0] == 0.0
                    && values[1] == 0.0
                    && values[2] > 0.0
                    && (values[2] - values[3]).abs() < 0.01 =>
            {
                values[2]
            }
            _ => return Ok(None),
        }
    } else if let Some(viewport) = viewport {
        viewport
    } else {
        return Ok(None);
    };
    let first_visible = root.children().find(|node| {
        node.is_element()
            && !matches!(
                node.tag_name().name(),
                "defs" | "style" | "title" | "desc" | "metadata"
            )
    });
    let Some(rectangle) = first_visible.filter(|node| node.has_tag_name("rect")) else {
        return Ok(None);
    };
    let attributes = resolve(rectangle, &root_style);
    if !no_effects(&attributes)
        || !super::is_full_canvas_white_background_rect(&attributes, canvas_width)
        || ["x", "y", "width", "height", "rx", "ry"].iter().any(|key| {
            attributes
                .get(*key)
                .is_some_and(|value| plain_dimension(value).is_none())
        })
    {
        return Ok(None);
    }
    let rx = attributes.get("rx").and_then(&plain_dimension);
    let ry = attributes.get("ry").and_then(&plain_dimension);
    if (attributes.contains_key("rx") && rx.is_none())
        || (attributes.contains_key("ry") && ry.is_none())
        || rx.is_some_and(|value| value < 0.0)
        || ry.is_some_and(|value| value < 0.0)
        || matches!((rx, ry), (Some(rx), Some(ry)) if (rx - ry).abs() >= 0.01)
    {
        return Ok(None);
    }
    let radius =
        rx.or(ry).unwrap_or(0.0).clamp(0.0, canvas_width / 2.0) * SAMPLE_SIZE as f64 / canvas_width;
    Ok(Some(ExporterCanvas {
        range: rectangle.range(),
        radius: radius as f32,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_keeps_holes_and_unifies_adjacent_runs() {
        let opaque = [10, 20, 30, 255];
        let clear = [0, 0, 0, 0];
        let rectangles = merge_sample_runs(
            &[
                opaque, opaque, clear, opaque, opaque, clear, clear, opaque, clear,
            ],
            3,
            3,
        );
        assert_eq!(rectangles.len(), 2);
        assert_eq!(
            (
                rectangles[0].x,
                rectangles[0].y,
                rectangles[0].width,
                rectangles[0].height
            ),
            (0, 0, 2, 2)
        );
        assert_eq!(
            (
                rectangles[1].x,
                rectangles[1].y,
                rectangles[1].width,
                rectangles[1].height
            ),
            (1, 2, 1, 1)
        );
    }

    #[test]
    fn template_removes_only_the_exporter_canvas() {
        let source = r##"<svg width="16" height="16" xmlns="http://www.w3.org/2000/svg"><defs><mask id="m"><rect width="16" height="16" fill="white"/></mask></defs><rect width="16" height="16" fill="white"/><path d="M4 4H12V12H4Z" fill="red"/><rect width="16" height="16" fill="white"/></svg>"##;
        let prepared = remove_canvas(source, exporter_canvas(source).unwrap().as_ref());
        assert_eq!(prepared.matches("<rect").count(), 2);
        assert!(prepared.contains("<mask id=\"m\"><rect"));
        assert!(prepared.contains("fill=\"red\"/><rect"));
    }

    #[test]
    fn exporter_canvas_uses_effective_css_paint() {
        for styles in [
            r#"fill="white" style="fill:red""#,
            r#"fill="white" class="red""#,
            r#"fill="white" id="red""#,
            r#"fill="white" opacity="0.5""#,
            r#"fill="white" style="opacity:50%""#,
            r#"fill="white" style="transform:translate(1px,1px)""#,
        ] {
            let source = format!(
                r#"<svg width="16" height="16" xmlns="http://www.w3.org/2000/svg"><style>.red,#red {{fill:red}}</style><rect width="16" height="16" {styles}/></svg>"#
            );
            assert!(exporter_canvas(&source).unwrap().is_none(), "{}", source);
        }
        let source = r#"<svg width="16" height="16" xmlns="http://www.w3.org/2000/svg"><style>.canvas {fill:white}</style><rect width="16" height="16" class="canvas"/></svg>"#;
        assert!(exporter_canvas(source).unwrap().is_some());
        assert!(
            !remove_canvas(source, exporter_canvas(source).unwrap().as_ref()).contains("<rect")
        );
    }

    #[test]
    fn exact_canvas_requires_square_untransformed_geometry() {
        for (root, rect) in [
            (r#"width="16" height="32""#, ""),
            (r#"width="16" height="16" viewBox="4 0 16 16""#, ""),
            (r#"width="16" height="16" viewBox="0 0 16 32""#, ""),
            (r#"width="16" height="16" opacity="0.5""#, ""),
            (r#"width="16" height="16""#, r#"rx="2" ry="4""#),
            (r#"width="16" height="16""#, r#"rx="25%""#),
        ] {
            let source = format!(
                r#"<svg {root} xmlns="http://www.w3.org/2000/svg"><rect width="16" height="16" fill="white" {rect}/></svg>"#
            );
            assert!(exporter_canvas(&source).unwrap().is_none(), "{}", source);
        }
    }

    #[test]
    fn exact_backing_preserves_white_detail_outside_rounded_corners() {
        let source = r#"<svg width="16" height="16" xmlns="http://www.w3.org/2000/svg"><rect width="16" height="16" rx="8" fill="white"/><rect width="2" height="2" fill="white"/></svg>"#;
        let icon = flatten_svg(source, true).unwrap();
        assert!(icon.contains("ROUND_RECT, 0, 0, 128, 128, 64,"));
        let preview = super::super::try_convert_chromium_icon_source_to_svg_markup_with_backdrop(
            &icon, "#111111",
        )
        .unwrap();
        let tree = usvg::Tree::from_str(&preview, &usvg::Options::default()).unwrap();
        let mut pixmap = Pixmap::new(SAMPLE_SIZE, SAMPLE_SIZE).unwrap();
        resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());
        assert_eq!(pixmap.pixels()[SAMPLE_SIZE as usize + 1].red(), 255);
        assert_eq!(pixmap.pixels()[SAMPLE_SIZE as usize - 2].red(), 17);
    }

    #[test]
    fn empty_rendering_is_valid_and_invalid_xml_is_rejected() {
        let empty = flatten_svg(
            r#"<svg width="16" height="16" xmlns="http://www.w3.org/2000/svg"/>"#,
            true,
        )
        .unwrap();
        assert!(empty.ends_with("MOVE_TO, 0, 0,\nCLOSE,\n"));
        assert!(flatten_svg("<svg>", true).is_err());
    }
}
