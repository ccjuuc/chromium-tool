//! Explicit, bounded approximation for SVG paints/compositing that Chromium's
//! vector-icon command set cannot express. Keeps the native canvas and viewport;
//! sampling quantizes color and opacity without removing background shapes.
use std::collections::BTreeMap;
use std::fmt::Write;

const SAMPLE_SIZE: u32 = 128;
type Color = [u8; 4];

#[derive(Clone, Copy)]
struct SpanRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn quantize(value: u8) -> u8 {
    ((u16::from(value) + 4) / 8 * 8).min(255) as u8
}

pub(super) fn flatten_tree(tree: &usvg::Tree, emit_path_colors: bool) -> Result<String, String> {
    let size = tree.size();
    let canvas = size.width().max(size.height()).ceil().max(1.0);
    let (dx, dy) = (
        (canvas - size.width()) / 2.0,
        (canvas - size.height()) / 2.0,
    );
    let scale = SAMPLE_SIZE as f32 / canvas;
    let transform =
        tiny_skia::Transform::from_scale(scale, scale).post_translate(dx * scale, dy * scale);
    let mut pixmap = tiny_skia::Pixmap::new(SAMPLE_SIZE, SAMPLE_SIZE)
        .ok_or_else(|| "无法创建近似转换画布".to_string())?;
    resvg::render(tree, transform, &mut pixmap.as_mut());
    // resvg renders into the supplied pixmap without clipping the root SVG
    // viewport. A square destination otherwise exposes artwork outside a
    // non-square viewport in what should remain transparent padding.
    let viewport = tiny_skia::Rect::from_xywh(0.0, 0.0, size.width(), size.height())
        .ok_or_else(|| "SVG 视口尺寸无效".to_string())?;
    let mut viewport_mask = tiny_skia::Mask::new(SAMPLE_SIZE, SAMPLE_SIZE)
        .ok_or_else(|| "无法创建近似转换裁剪区域".to_string())?;
    viewport_mask.fill_path(
        &tiny_skia::PathBuilder::from_rect(viewport),
        tiny_skia::FillRule::Winding,
        false,
        transform,
    );
    pixmap.apply_mask(&viewport_mask);
    let mut rectangles: BTreeMap<Color, Vec<SpanRect>> = BTreeMap::new();
    let mut active: BTreeMap<(u32, u32, Color), SpanRect> = BTreeMap::new();
    let color_at = |x, y| {
        let pixel = pixmap.pixel(x, y).unwrap().demultiply();
        let rgb = if emit_path_colors {
            [
                quantize(pixel.red()),
                quantize(pixel.green()),
                quantize(pixel.blue()),
            ]
        } else {
            [0, 0, 0]
        };
        [rgb[0], rgb[1], rgb[2], quantize(pixel.alpha())]
    };
    for y in 0..SAMPLE_SIZE {
        let mut next = BTreeMap::new();
        let mut x = 0;
        while x < SAMPLE_SIZE {
            let color = color_at(x, y);
            let start = x;
            x += 1;
            while x < SAMPLE_SIZE && color_at(x, y) == color {
                x += 1;
            }
            if color[3] == 0 {
                continue;
            }
            let key = (start, x, color);
            let rect = if let Some(mut rect) = active.remove(&key) {
                rect.height += 1;
                rect
            } else {
                SpanRect {
                    x: start,
                    y,
                    width: x - start,
                    height: 1,
                }
            };
            next.insert(key, rect);
        }
        for ((_, _, color), rect) in active {
            rectangles.entry(color).or_default().push(rect);
        }
        active = next;
    }
    for ((_, _, color), rect) in active {
        rectangles.entry(color).or_default().push(rect);
    }
    let mut output = format!(
        "// Sampled at {SAMPLE_SIZE}x{SAMPLE_SIZE}; RGBA quantized to steps of 8.\nCANVAS_DIMENSIONS, {},\nCLIP, {}, {}, {}, {},\n",
        super::format_number(canvas), super::format_number(dx), super::format_number(dy),
        super::format_number(size.width()), super::format_number(size.height()),
    );
    let pixel_unit = canvas / SAMPLE_SIZE as f32;
    for (index, (color, rects)) in rectangles.iter().enumerate() {
        if index > 0 {
            output.push_str("NEW_PATH,\n");
        }
        if emit_path_colors {
            writeln!(
                output,
                "PATH_COLOR_ARGB, 0x{:02X}, 0x{:02X}, 0x{:02X}, 0x{:02X},",
                color[3], color[0], color[1], color[2]
            )
            .unwrap();
        } else {
            writeln!(output, "PATH_COLOR_ALPHA, {},", color[3]).unwrap();
        }
        // Rectangles describe disjoint samples. Disable AA so shared edges do
        // not acquire seams or extra opacity when painted as separate paths.
        output.push_str("FILL_RULE_NONZERO,\nDISABLE_AA,\n");
        for rect in rects {
            let left = super::format_number(rect.x as f32 * pixel_unit);
            let top = super::format_number(rect.y as f32 * pixel_unit);
            let right = super::format_number((rect.x + rect.width) as f32 * pixel_unit);
            let bottom = super::format_number((rect.y + rect.height) as f32 * pixel_unit);
            writeln!(
                output,
                "MOVE_TO, {}, {},\nLINE_TO, {}, {},\nLINE_TO, {}, {},\nLINE_TO, {}, {},\nCLOSE,",
                left, top, right, top, right, bottom, left, bottom
            )
            .unwrap();
        }
    }
    if rectangles.is_empty() {
        output.push_str("PATH_COLOR_ALPHA, 0,\nMOVE_TO, 0, 0,\nCLOSE,\n");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overflow_gradient(width: f32, height: f32) -> usvg::Tree {
        let source = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}">
<defs><linearGradient id="g"><stop stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient></defs>
<rect x="-16" y="-16" width="64" height="64" fill="url(#g)"/>
</svg>"##
        );
        usvg::Tree::from_str(&source, &usvg::Options::default()).unwrap()
    }

    fn render(icon: &str) -> tiny_skia::Pixmap {
        let markup = super::super::reverse::convert(icon, None).unwrap();
        let tree = usvg::Tree::from_str(&markup, &usvg::Options::default()).unwrap();
        let scale = 128.0 / tree.size().width();
        let mut image = tiny_skia::Pixmap::new(128, 128).unwrap();
        resvg::render(
            &tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut image.as_mut(),
        );
        image
    }

    #[test]
    fn approximation_keeps_native_size_and_clips_overflow_out_of_padding() {
        let tree = overflow_gradient(16.0, 8.0);
        for emit_path_colors in [true, false] {
            let icon = flatten_tree(&tree, emit_path_colors).unwrap();
            assert!(icon.contains("CANVAS_DIMENSIONS, 16,\n"));
            let image = render(&icon);
            // A 16x8 viewport is centered in the 16x16 native canvas.
            for y in 0..128 {
                assert_eq!(
                    image.pixel(64, y).unwrap().alpha(),
                    if (32..96).contains(&y) { 255 } else { 0 },
                    "row {y}"
                );
            }
        }
    }

    #[test]
    fn fractional_viewport_uses_same_ceil_canvas_and_centering_as_vector_output() {
        let icon = flatten_tree(&overflow_gradient(15.25, 7.5), true).unwrap();
        assert!(icon.contains("CANVAS_DIMENSIONS, 16,\n"));
        let image = render(&icon);
        // Native viewport [0.375, 15.625] x [4.25, 11.75], rendered at 8x.
        for y in 0..128 {
            for x in 0..128 {
                let inside = (3..125).contains(&x) && (34..94).contains(&y);
                assert_eq!(
                    image.pixel(x, y).unwrap().alpha(),
                    if inside { 255 } else { 0 },
                    "pixel ({x}, {y})"
                );
            }
        }
    }
}
