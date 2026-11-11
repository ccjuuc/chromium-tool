use icns::{IconFamily, IconType, Image as IcnsImage};
use ico::{IconDir, IconImage};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageReader, Rgba};
use pic_scale::{
    ImageSize, ImageStore, ResamplingFunction, Scaler, Scaling, ScalingF32, ThreadingPolicy,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    vec,
};
pub fn apply_rounded_corners(fpath: &str, radius: &str) -> String {
    let parse_r = |s: &str| -> u32 { s.trim().parse::<u32>().expect("Failed to parse radius") };
    let (
        mut left_top_radius,
        mut right_top_radius,
        mut left_bottom_radius,
        mut right_bottom_radius,
    ) = {
        let radius_vec: Vec<&str> = radius.split(',').collect();
        if radius_vec.len() == 4 {
            (
                parse_r(radius_vec[0]),
                parse_r(radius_vec[1]),
                parse_r(radius_vec[2]),
                parse_r(radius_vec[3]),
            )
        } else if radius_vec.len() == 2 {
            let top = parse_r(radius_vec[0]);
            let bottom = parse_r(radius_vec[1]);
            (top, top, bottom, bottom)
        } else {
            let r = parse_r(radius);
            (r, r, r, r)
        }
    };

    let img = image::open(fpath).expect("Failed to open input image");
    let (width, height) = img.dimensions();
    let max_radius = width.min(height) / 2;
    left_top_radius = left_top_radius.min(max_radius);
    right_top_radius = right_top_radius.min(max_radius);
    left_bottom_radius = left_bottom_radius.min(max_radius);
    right_bottom_radius = right_bottom_radius.min(max_radius);

    let scale_factor = 4;
    let scaled_width = width * scale_factor;
    let scaled_height = height * scale_factor;

    let scaled_img = img.resize_exact(
        scaled_width,
        scaled_height,
        image::imageops::FilterType::Lanczos3,
    );

    let mut output = ImageBuffer::new(scaled_width, scaled_height);
    let scaled_radii = vec![
        left_top_radius * scale_factor,
        right_top_radius * scale_factor,
        left_bottom_radius * scale_factor,
        right_bottom_radius * scale_factor,
    ];

    for y in 0..scaled_height {
        for x in 0..scaled_width {
            let pixel = scaled_img.get_pixel(x, y);
            if is_inside_rounded_corner(x, y, scaled_width, scaled_height, &scaled_radii) {
                output.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            } else {
                output.put_pixel(x, y, pixel);
            }
        }
    }

    let rounded_img = DynamicImage::ImageRgba8(output).resize_exact(
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    );
    let input_path = Path::new(fpath);
    let mut output_path = PathBuf::from(input_path.parent().unwrap_or_else(|| Path::new(".")));
    let file_stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let extension = input_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("png");
    let output_file_name = format!("{}_radius.{}", file_stem, extension);
    output_path.push(output_file_name);

    let output_path_str = output_path.to_string_lossy().to_string();
    rounded_img.save(output_path).expect("Failed to save image");
    output_path_str
}

fn is_inside_rounded_corner(x: u32, y: u32, width: u32, height: u32, radius: &[u32]) -> bool {
    let left_top_radius = radius[0];
    let right_top_radius = radius[1];
    let left_bottom_radius = radius[2];
    let right_bottom_radius = radius[3];
    if x < left_top_radius && y < left_top_radius {
        let dx = left_top_radius as i64 - x as i64;
        let dy = left_top_radius as i64 - y as i64;
        let r = left_top_radius as i64;
        return dx * dx + dy * dy > r * r;
    }

    if x >= width.saturating_sub(right_top_radius) && y < right_top_radius {
        let dx = x as i64 - width.saturating_sub(right_top_radius) as i64;
        let dy = right_top_radius as i64 - y as i64;
        let r = right_top_radius as i64;
        return dx * dx + dy * dy > r * r;
    }

    if x < left_bottom_radius && y >= height.saturating_sub(left_bottom_radius) {
        let dx = left_bottom_radius as i64 - x as i64;
        let dy = y as i64 - height.saturating_sub(left_bottom_radius) as i64;
        let r = left_bottom_radius as i64;
        return dx * dx + dy * dy > r * r;
    }

    if x >= width.saturating_sub(right_bottom_radius)
        && y >= height.saturating_sub(right_bottom_radius)
    {
        let dx = x as i64 - width.saturating_sub(right_bottom_radius) as i64;
        let dy = y as i64 - height.saturating_sub(right_bottom_radius) as i64;
        let r = right_bottom_radius as i64;
        return dx * dx + dy * dy > r * r;
    }
    false
}

static OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Write a complete result beside its destination, then replace the destination.
/// An encoding/validation failure never truncates a previously successful file.
pub(crate) fn write_output_atomic(output: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let name = output
        .file_name()
        .ok_or_else(|| "Invalid output filename".to_string())?;
    let (temporary, mut file) = loop {
        let sequence = OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{}.{}.{sequence}.tmp",
            name.to_string_lossy(),
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Failed to create output in '{}': {error}",
                    parent.display()
                ))
            }
        }
    };
    let result = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    let result = result.and_then(|_| std::fs::rename(&temporary, output));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("Failed to save '{}': {error}", output.display()));
    }
    Ok(())
}

fn load_rgba_image(fpath: &str) -> Result<DynamicImage, String> {
    if Path::new(fpath)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        let content =
            std::fs::read_to_string(fpath).map_err(|e| format!("Failed to read SVG: {e}"))?;
        let pixmap = super::svg_png::render_svg(&content)?;
        // tiny-skia stores premultiplied RGBA; image and icon encoders require straight RGBA.
        let pixels: Vec<u8> = pixmap
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let color = pixel.demultiply();
                [color.red(), color.green(), color.blue(), color.alpha()]
            })
            .collect();
        let image = ImageBuffer::from_raw(pixmap.width(), pixmap.height(), pixels)
            .ok_or_else(|| "Invalid rendered SVG dimensions".to_string())?;
        return Ok(DynamicImage::ImageRgba8(image));
    }
    let image = ImageReader::open(fpath)
        .map_err(|e| format!("Failed to open image '{fpath}': {e}"))?
        .decode()
        .map_err(|e| format!("Failed to decode image '{fpath}': {e}"))?;
    Ok(DynamicImage::ImageRgba8(image.to_rgba8()))
}

fn resize_rgba(image: &DynamicImage, width: u32, height: u32) -> Result<image::RgbaImage, String> {
    if width == 0 || height == 0 || width as u64 * height as u64 > 64 * 1024 * 1024 {
        return Err("Image dimensions must be nonzero and at most 64 megapixels".to_string());
    }
    let dimensions = image.dimensions();
    let rgba = image.to_rgba8();
    if dimensions == (width, height) {
        return Ok(rgba);
    }
    // LinearScaler in pic-scale 0.3 premultiplies sRGB in u8 before converting
    // to linear light. That loses low-alpha colors and darkens translucent
    // edges. Keep the transfer conversion and premultiplication in f32 until
    // the resized image is converted back to straight sRGB for encoding.
    let linear: [f32; 256] = std::array::from_fn(|channel| {
        let srgb = channel as f32 / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    });
    let pixels: Vec<f32> = rgba
        .pixels()
        .flat_map(|pixel| {
            let alpha = pixel[3] as f32 / 255.0;
            [
                linear[pixel[0] as usize] * alpha,
                linear[pixel[1] as usize] * alpha,
                linear[pixel[2] as usize] * alpha,
                alpha,
            ]
        })
        .collect();
    let store = ImageStore::<f32, 4>::new(pixels, dimensions.0 as usize, dimensions.1 as usize)
        .map_err(|e| format!("Failed to create RGBA image storage: {e}"))?;
    let mut scaler = Scaler::new(ResamplingFunction::Lanczos3);
    scaler.set_threading_policy(ThreadingPolicy::Adaptive);
    let resized = scaler
        .resize_rgba_f32(
            ImageSize::new(width as usize, height as usize),
            store,
            false,
        )
        .map_err(|e| format!("Failed to resize image: {e}"))?;
    let bytes = resized
        .as_bytes()
        .chunks_exact(4)
        .flat_map(|pixel| {
            let alpha = pixel[3];
            if alpha <= f32::EPSILON {
                return [0, 0, 0, 0];
            }
            let encode = |channel: f32| {
                let value = (channel / alpha).clamp(0.0, 1.0);
                let srgb = if value <= 0.0031308 {
                    value * 12.92
                } else {
                    1.055 * value.powf(1.0 / 2.4) - 0.055
                };
                (srgb * 255.0).round().clamp(0.0, 255.0) as u8
            };
            [
                encode(pixel[0]),
                encode(pixel[1]),
                encode(pixel[2]),
                (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
            ]
        })
        .collect();
    ImageBuffer::from_raw(width, height, bytes)
        .ok_or_else(|| "Invalid resized RGBA image data".to_string())
}

/// Fit the source into a square icon while preserving aspect ratio and alpha.
fn square_icon(
    image: &DynamicImage,
    size: u32,
    content_size: u32,
) -> Result<image::RgbaImage, String> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || content_size == 0 || content_size > size {
        return Err("Invalid icon dimensions".to_string());
    }
    let scale = content_size as f64 / width.max(height) as f64;
    let resized_width = ((width as f64 * scale).round() as u32).clamp(1, content_size);
    let resized_height = ((height as f64 * scale).round() as u32).clamp(1, content_size);
    let resized = resize_rgba(image, resized_width, resized_height)?;
    let mut canvas = image::RgbaImage::new(size, size);
    image::imageops::replace(
        &mut canvas,
        &resized,
        i64::from((size - resized_width) / 2),
        i64::from((size - resized_height) / 2),
    );
    Ok(canvas)
}

pub fn resize_image_with_scaler(
    fpath: &str,
    out_path: Option<&str>,
    width: u32,
    height: u32,
) -> Option<DynamicImage> {
    let result: Result<Option<DynamicImage>, String> = (|| {
        let image = load_rgba_image(fpath)?;
        let image = DynamicImage::ImageRgba8(resize_rgba(&image, width, height)?);
        if let Some(out_path) = out_path {
            let output = Path::new(fpath)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(out_path);
            image
                .save(&output)
                .map_err(|e| format!("Failed to save '{}': {e}", output.display()))?;
            Ok(None)
        } else {
            Ok(Some(image))
        }
    })();
    match result {
        Ok(image) => image,
        Err(error) => {
            tracing::error!("{error}");
            None
        }
    }
}

pub fn generate_chromium_logo(fpath: &str, out_path: &str, canvas_size: u32, logo_size: u32) {
    let resize_img = resize_image_with_scaler(fpath, None, logo_size, logo_size);
    if let Some(resize_img) = resize_img {
        let canvas = ImageBuffer::from_pixel(canvas_size, canvas_size, Rgba([0, 0, 0, 0]));
        let x = (canvas_size - logo_size) / 2;
        let y = (canvas_size - logo_size) / 2;
        let mut output = canvas.clone();
        for sy in 0..logo_size {
            for sx in 0..logo_size {
                let pixel = resize_img.get_pixel(sx, sy);
                output.put_pixel(x + sx, y + sy, pixel);
            }
        }

        let input_path = Path::new(fpath);
        let mut output_path = PathBuf::from(input_path.parent().unwrap());
        output_path.push(out_path);
        DynamicImage::ImageRgba8(output)
            .save(&output_path)
            .expect("Failed to save image");
    }
}

pub fn try_generate_chromium_ico(fpath: &str, out_path: &str) -> Result<String, String> {
    let image = load_rgba_image(fpath)?;
    let output_path = Path::new(fpath)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(out_path);
    let mut icon_dir = IconDir::new(ico::ResourceType::Icon);
    for size in [256, 128, 64, 48, 32, 24, 16] {
        let rgba = square_icon(&image, size, size)?;
        let icon_image = IconImage::from_rgba_data(size, size, rgba.into_raw());
        icon_dir.add_entry(
            ico::IconDirEntry::encode(&icon_image)
                .map_err(|e| format!("Failed to encode {size}px ICO entry: {e}"))?,
        );
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    icon_dir
        .write(&mut bytes)
        .map_err(|e| format!("Failed to encode ICO: {e}"))?;
    write_output_atomic(&output_path, &bytes.into_inner())?;
    Ok(output_path.to_string_lossy().into_owned())
}

pub fn generate_chromium_ico(fpath: &str, out_path: &str) -> String {
    try_generate_chromium_ico(fpath, out_path).expect("Failed to generate Chromium ICO")
}

pub fn generate_chromium_document_icns(fpath: &str, out_path: &str) -> String {
    let input_path = Path::new(fpath);
    let output_path = PathBuf::from(input_path.parent().unwrap());
    let logo_add = output_path.join("product_logo_192.png");
    let mut img = image::open(fpath).expect("Failed to open image");
    if img.width() != 256 {
        img = resize_image_with_scaler(fpath, None, 256, 256).unwrap();
    }
    let logo = image::open(logo_add).expect("Failed to open logo");
    let (width, height) = img.dimensions();
    let (logo_width, logo_height) = logo.dimensions();
    let x = (width - logo_width) / 2;
    let y = height - logo_height;
    image::imageops::overlay(&mut img, &logo, x as i64, y as i64);
    let document_path = output_path.join("tmp.png");
    img.save(&document_path).expect("Failed to save image");

    let result = generate_chromium_icns(document_path.to_str().unwrap(), out_path, false);

    // 清理临时文件
    if document_path.exists() {
        let _ = std::fs::remove_file(&document_path);
    }

    result
}

pub fn try_generate_chromium_icns(
    fpath: &str,
    out_path: &str,
    border: bool,
) -> Result<String, String> {
    let image = load_rgba_image(fpath)?;
    let output_path = Path::new(fpath)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(out_path);

    // Match Chromium's makeicns2 convention. PNG-in-icp4/icp5/icp6 can
    // scramble small icons on macOS; legacy RGB entries include a separate
    // alpha mask. There is no legacy 64px slot, so omit that representation.
    // https://chromium.googlesource.com/chromium/src/+/main/docs/mac/icons.md
    let representations = [
        (512, IconType::RGBA32_512x512),
        (256, IconType::RGBA32_256x256),
        (128, IconType::RGB24_128x128),
        (32, IconType::RGB24_32x32),
        (16, IconType::RGB24_16x16),
    ];
    let mut icon_family = IconFamily::new();

    for (size, icon_type) in representations {
        let content_size = if border {
            (size * 208 + 128) / 256
        } else {
            size
        };
        let rgba = square_icon(&image, size, content_size)?.into_raw();
        let icns_image = IcnsImage::from_data(icns::PixelFormat::RGBA, size, size, rgba)
            .map_err(|e| format!("Failed to create {size}px ICNS image: {e}"))?;
        // add_icon_with_type writes s8mk/l8mk/t8mk along with legacy RGB.
        icon_family
            .add_icon_with_type(&icns_image, icon_type)
            .map_err(|e| format!("Failed to encode {size}px ICNS entry: {e}"))?;
    }
    let mut bytes = Vec::new();
    icon_family
        .write(&mut bytes)
        .map_err(|e| format!("Failed to encode ICNS: {e}"))?;
    write_output_atomic(&output_path, &bytes)?;
    Ok(output_path.to_string_lossy().into_owned())
}

pub fn generate_chromium_icns(fpath: &str, out_path: &str, border: bool) -> String {
    try_generate_chromium_icns(fpath, out_path, border).expect("Failed to generate Chromium ICNS")
}

#[allow(dead_code)]
pub fn generate_nine_patch_with_corners(fpath: &str, radius: &str) {
    let img = image::open(fpath).expect("Failed to open input image");
    let (width, height) = img.dimensions();
    let input_path = Path::new(fpath);
    let mut output_path = PathBuf::from(input_path.parent().unwrap());
    let file_stem = input_path.file_stem().unwrap().to_str().unwrap();
    let extension = input_path.extension().unwrap().to_str().unwrap();

    let (left_top_radius, right_top_radius, left_bottom_radius, right_bottom_radius) =
        if radius.contains(",") {
            let radius_vec: Vec<&str> = radius.split(',').collect();
            (
                radius_vec[0].parse().expect("Failed to parse radius"),
                radius_vec[1].parse().expect("Failed to parse radius"),
                radius_vec[2].parse().expect("Failed to parse radius"),
                radius_vec[3].parse().expect("Failed to parse radius"),
            )
        } else {
            let r = radius.parse().expect("Failed to parse radius");
            (r, r, r, r)
        };

    let mut output = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(width, height);

    let parts = [
        ("top_left", 0, 0, left_top_radius, left_top_radius),
        (
            "top_center",
            left_top_radius,
            0,
            width - left_top_radius - right_top_radius,
            left_top_radius,
        ),
        (
            "top_right",
            width - right_top_radius,
            0,
            right_top_radius,
            right_top_radius,
        ),
        (
            "middle_left",
            0,
            left_top_radius,
            left_top_radius,
            height - left_top_radius - left_bottom_radius,
        ),
        (
            "middle_center",
            left_top_radius,
            left_top_radius,
            width - left_top_radius - right_top_radius,
            height - left_top_radius - left_bottom_radius,
        ),
        (
            "middle_right",
            width - right_top_radius,
            left_top_radius,
            right_top_radius,
            height - right_top_radius - right_bottom_radius,
        ),
        (
            "bottom_left",
            0,
            height - left_bottom_radius,
            left_bottom_radius,
            left_bottom_radius,
        ),
        (
            "bottom_center",
            left_bottom_radius,
            height - left_bottom_radius,
            width - left_bottom_radius - right_bottom_radius,
            left_bottom_radius,
        ),
        (
            "bottom_right",
            width - right_bottom_radius,
            height - right_bottom_radius,
            right_bottom_radius,
            right_bottom_radius,
        ),
    ];

    for (_name, x, y, w, h) in &parts {
        let sub_img = img.view(*x, *y, *w, *h).to_image();
        for sy in 0..*h {
            for sx in 0..*w {
                let pixel = sub_img.get_pixel(sx, sy);
                output.put_pixel(x + sx, y + sy, *pixel);
            }
        }
    }

    for x in 0..width {
        output.put_pixel(x, left_top_radius, Rgba([0, 0, 0, 255]));
        output.put_pixel(x, height - left_top_radius, Rgba([0, 0, 0, 255]));
    }

    for y in 0..height {
        output.put_pixel(left_top_radius, y, Rgba([0, 0, 0, 255]));
        output.put_pixel(width - left_top_radius, y, Rgba([0, 0, 0, 255]));
    }

    let output_file_name = format!("{}_nine_patch.{}", file_stem, extension);
    output_path.push(&output_file_name);
    DynamicImage::ImageRgba8(output)
        .save(&output_path)
        .expect("Failed to save image");
}

pub fn generate_grayscale_image(fpath: &str, out_path: &str, size: u32) {
    if let Some(resized_img) = resize_image_with_scaler(fpath, None, size, size) {
        let grayscale_img = resized_img.to_luma_alpha8();
        let input_path = Path::new(fpath);
        let mut output_path = PathBuf::from(input_path.parent().unwrap());
        output_path.push(out_path);
        DynamicImage::ImageLumaA8(grayscale_img)
            .save(&output_path)
            .expect("Failed to save image");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory(label: &str) -> PathBuf {
        let sequence = OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "chromium_{label}_{}_{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn assert_color(actual: &[u8], expected: [u8; 4], context: &str) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                actual.abs_diff(expected) <= 2,
                "{context}: unexpected color {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn resizing_preserves_low_alpha_color_and_ignores_hidden_edge_colors() {
        for alpha in [1, 8, 64, 128, 255] {
            let source = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
                13,
                7,
                Rgba([64, 128, 192, alpha]),
            ));
            let output = resize_rgba(&source, 64, 32).unwrap();
            assert_eq!(output.get_pixel(32, 16).0, [64, 128, 192, alpha]);
        }
        let edge = DynamicImage::ImageRgba8(ImageBuffer::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 0])
            }
        }));
        let resized = resize_rgba(&edge, 32, 1).unwrap();
        assert!(resized.pixels().any(|pixel| pixel[3] > 0 && pixel[3] < 255));
        for pixel in resized.pixels().filter(|pixel| pixel[3] > 0) {
            assert_eq!(
                &pixel.0[..3],
                &[255, 0, 0],
                "transparent blue must not contaminate the visible red edge"
            );
        }
        let black_white = DynamicImage::ImageRgba8(ImageBuffer::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        }));
        let average = resize_rgba(&black_white, 1, 1).unwrap();
        assert_eq!(
            average.get_pixel(0, 0).0,
            [188, 188, 188, 255],
            "resampling must average light in linear space"
        );
    }

    #[test]
    fn ico_and_icns_accept_rgb_gray_and_rgba_sources() {
        let directory = test_directory("color_formats");
        for (name, source_image, expected) in [
            (
                "rgb",
                DynamicImage::ImageRgb8(ImageBuffer::from_pixel(
                    32,
                    32,
                    image::Rgb([64, 128, 192]),
                )),
                [64, 128, 192, 255],
            ),
            (
                "gray",
                DynamicImage::ImageLuma8(ImageBuffer::from_pixel(32, 32, image::Luma([96]))),
                [96, 96, 96, 255],
            ),
            (
                "rgba",
                DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
                    32,
                    32,
                    Rgba([64, 128, 192, 128]),
                )),
                [64, 128, 192, 128],
            ),
        ] {
            let source = directory.join(format!("{name}.png"));
            source_image.save(&source).unwrap();
            let ico_path =
                try_generate_chromium_ico(source.to_str().unwrap(), &format!("{name}.ico"))
                    .unwrap();
            let ico =
                IconDir::read(std::io::Cursor::new(std::fs::read(ico_path).unwrap())).unwrap();
            assert_eq!(ico.entries().len(), 7);
            for entry in ico.entries() {
                let image = entry.decode().unwrap();
                let offset =
                    ((image.height() / 2 * image.width() + image.width() / 2) * 4) as usize;
                assert_color(
                    &image.rgba_data()[offset..offset + 4],
                    expected,
                    &format!("{name} ICO {}px", image.width()),
                );
            }
            let icns_path = try_generate_chromium_icns(
                source.to_str().unwrap(),
                &format!("{name}.icns"),
                false,
            )
            .unwrap();
            let icns =
                IconFamily::read(std::io::Cursor::new(std::fs::read(icns_path).unwrap())).unwrap();
            for kind in [
                IconType::RGB24_16x16,
                IconType::RGB24_128x128,
                IconType::RGBA32_256x256,
            ] {
                let image = icns
                    .get_icon_with_type(kind)
                    .unwrap()
                    .convert_to(icns::PixelFormat::RGBA);
                let offset =
                    ((image.height() / 2 * image.width() + image.width() / 2) * 4) as usize;
                assert_color(
                    &image.data()[offset..offset + 4],
                    expected,
                    &format!("{name} ICNS {}px", image.width()),
                );
            }
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn small_and_nonsquare_icns_preserve_aspect_ratio_and_padding() {
        let directory = test_directory("icon_padding");
        for (width, height, expected_width, expected_height) in
            [(32, 32, 208, 208), (32, 16, 208, 104), (257, 64, 208, 52)]
        {
            let source = directory.join(format!("{width}x{height}.png"));
            ImageBuffer::from_pixel(width, height, Rgba([20u8, 40, 60, 255]))
                .save(&source)
                .unwrap();
            let output =
                try_generate_chromium_icns(source.to_str().unwrap(), "padded.icns", true).unwrap();
            let family =
                IconFamily::read(std::io::Cursor::new(std::fs::read(output).unwrap())).unwrap();
            let image = family
                .get_icon_with_type(IconType::RGBA32_256x256)
                .unwrap()
                .convert_to(icns::PixelFormat::RGBA);
            let left = (256 - expected_width) / 2;
            let top = (256 - expected_height) / 2;
            for y in 0..256 {
                for x in 0..256 {
                    let alpha = image.data()[((y * 256 + x) * 4 + 3) as usize];
                    let inside = x >= left
                        && x < left + expected_width
                        && y >= top
                        && y < top + expected_height;
                    assert_eq!(
                        alpha,
                        if inside { 255 } else { 0 },
                        "wrong padding at ({x}, {y}) for {width}x{height}"
                    );
                }
            }
        }
        assert!(!directory.join("tmp_app.png").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_icon_conversion_preserves_existing_files() {
        let directory = test_directory("invalid_input");
        let source = directory.join("broken.png");
        std::fs::write(&source, b"invalid image").unwrap();
        for name in ["result.ico", "result.icns"] {
            std::fs::write(directory.join(name), b"previous output").unwrap();
        }
        assert!(try_generate_chromium_ico(source.to_str().unwrap(), "result.ico").is_err());
        assert!(try_generate_chromium_icns(source.to_str().unwrap(), "result.icns", true).is_err());
        for name in ["result.ico", "result.icns"] {
            assert_eq!(
                std::fs::read(directory.join(name)).unwrap(),
                b"previous output"
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn chromium_icns_uses_legacy_small_icons_and_preserves_rgba() {
        let directory =
            std::env::temp_dir().join(format!("chromium_icns_encoding_{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source.png");
        let pixels = ImageBuffer::from_fn(512, 512, |x, y| {
            Rgba([
                x as u8,
                y as u8,
                (x ^ y) as u8,
                [0, 64, 128, 255][(x / 128) as usize],
            ])
        });
        pixels.save(&source).unwrap();

        for border in [false, true] {
            let output = generate_chromium_icns(source.to_str().unwrap(), "test.icns", border);
            let bytes = std::fs::read(output).unwrap();
            assert_eq!(&bytes[..4], b"icns");
            assert_eq!(
                u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize,
                bytes.len()
            );
            let family = IconFamily::read(std::io::Cursor::new(&bytes)).unwrap();
            let codes: Vec<_> = family
                .elements
                .iter()
                .map(|element| element.ostype.0)
                .collect();
            assert_eq!(
                codes,
                [*b"ic09", *b"ic08", *b"it32", *b"t8mk", *b"il32", *b"l8mk", *b"is32", *b"s8mk"]
            );

            for (size, kind, mask) in [
                (16, IconType::RGB24_16x16, Some(IconType::Mask8_16x16)),
                (32, IconType::RGB24_32x32, Some(IconType::Mask8_32x32)),
                (128, IconType::RGB24_128x128, Some(IconType::Mask8_128x128)),
                (256, IconType::RGBA32_256x256, None),
                (512, IconType::RGBA32_512x512, None),
            ] {
                let decoded = family
                    .get_icon_with_type(kind)
                    .unwrap()
                    .convert_to(icns::PixelFormat::RGBA);
                assert_eq!((decoded.width(), decoded.height()), (size, size));
                let element = family
                    .elements
                    .iter()
                    .find(|element| element.ostype == kind.ostype())
                    .unwrap();
                if let Some(mask) = mask {
                    assert!(!element.data.starts_with(b"\x89PNG\r\n\x1a\n"));
                    let mask_data = &family
                        .elements
                        .iter()
                        .find(|element| element.ostype == mask.ostype())
                        .unwrap()
                        .data;
                    let alpha: Vec<_> = decoded
                        .data()
                        .chunks_exact(4)
                        .map(|pixel| pixel[3])
                        .collect();
                    assert_eq!(mask_data, &alpha);
                    assert_eq!(mask_data.len(), (size * size) as usize);
                    if size == 128 {
                        assert_eq!(
                            &element.data[..4],
                            &[0, 0, 0, 0],
                            "it32 requires the legacy prefix"
                        );
                    }
                } else {
                    assert!(element.data.starts_with(b"\x89PNG\r\n\x1a\n"));
                }
                if border {
                    // Lanczos may leave a one-level alpha fringe after downsampling.
                    assert!(
                        decoded.data()[((size - 1) * 4 + 3) as usize] <= 2,
                        "app icon padding must remain transparent"
                    );
                    assert!(decoded.data()[((size / 2 * size + size / 2) * 4 + 3) as usize] > 0);
                } else {
                    let expected =
                        resize_image_with_scaler(source.to_str().unwrap(), None, size, size)
                            .unwrap()
                            .to_rgba8();
                    assert_eq!(
                        decoded.data(),
                        expected.as_raw(),
                        "RGBA mismatch at {}px",
                        size
                    );
                }
            }
        }
        assert!(!directory.join("tmp_app.png").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
