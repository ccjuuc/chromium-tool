use resvg::tiny_skia::{Pixmap, Transform};

pub(crate) fn render_svg(content: &str) -> Result<Pixmap, String> {
    let tree = super::svg_document::parse_svg(content)?;
    let size = tree.size();
    let width = size.width().ceil() as u32;
    let height = size.height().ceil() as u32;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 64 * 1024 * 1024 {
        return Err("SVG 栅格尺寸必须大于零且不超过 6400 万像素".to_string());
    }
    let mut pixmap =
        Pixmap::new(width, height).ok_or_else(|| "无法创建 SVG 渲染画布".to_string())?;
    // Integer output bounds contain the original viewport without stretching.
    resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());
    Ok(pixmap)
}

pub fn try_convert_svg_to_png(svg_path: &str, out_path: &str) -> Result<String, String> {
    let content = std::fs::read_to_string(svg_path)
        .map_err(|e| format!("Failed to read SVG '{svg_path}': {e}"))?;
    let pixmap = render_svg(&content)?;
    let bytes = pixmap
        .encode_png()
        .map_err(|e| format!("Failed to encode PNG: {e}"))?;
    let output = std::path::Path::new(svg_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(out_path);
    super::image_util::write_output_atomic(&output, &bytes)?;
    Ok(output.to_string_lossy().into_owned())
}

pub fn convert_svg_to_png(svg_path: &str, out_path: &str) -> String {
    try_convert_svg_to_png(svg_path, out_path).expect("Failed to convert SVG to PNG")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figma_sun_png_has_all_eight_rays_and_transparent_center() {
        let source = include_str!("fixtures/light_mode_use.svg").replacen(
            "width=\"16\" height=\"16\"",
            "width=\"160\" height=\"160\"",
            1,
        );
        let image = render_svg(&source).unwrap();
        for (x, y) in [
            (80, 10),
            (130, 30),
            (150, 80),
            (130, 130),
            (80, 150),
            (30, 130),
            (10, 80),
            (30, 30),
        ] {
            let visible = ((y - 5)..=(y + 5))
                .any(|py| ((x - 5)..=(x + 5)).any(|px| image.pixel(px, py).unwrap().alpha() > 128));
            assert!(visible, "missing ray at ({x}, {y})");
        }
        assert_eq!(image.pixel(80, 80).unwrap().alpha(), 0);
    }

    #[test]
    fn failed_svg_conversion_preserves_existing_output() {
        let directory = std::env::temp_dir().join(format!("svg_png_atomic_{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("bad.svg");
        let output = directory.join("result.png");
        std::fs::write(&source, "<svg>").unwrap();
        std::fs::write(&output, b"previous output").unwrap();
        assert!(try_convert_svg_to_png(source.to_str().unwrap(), "result.png").is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
