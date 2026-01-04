use image::{DynamicImage, GenericImageView, ImageBuffer, ImageReader, Rgba};
use std::{fs::File, path::{Path, PathBuf}, vec};
use ico::{IconDir, IconImage};
use icns::{IconFamily, IconType, Image as IcnsImage};
use pic_scale::{
  ImageSize, ImageStore, LinearScaler, ResamplingFunction, Scaling, ThreadingPolicy
};
pub fn apply_rounded_corners(fpath: &str, radius: &str) -> String {
    let left_top_radius : u32;
    let right_top_radius;
    let left_bottom_radius;
    let right_bottom_radius;
    
    let radius_vec: Vec<&str> = radius.split(",").collect();
    if radius_vec.len() == 4 {
        left_top_radius = radius_vec[0].parse().expect("Failed to parse radius");
        right_top_radius = radius_vec[1].parse().expect("Failed to parse radius");
        left_bottom_radius = radius_vec[2].parse().expect("Failed to parse radius");
        right_bottom_radius = radius_vec[3].parse().expect("Failed to parse radius");
    } else if radius_vec.len() == 2 {
        left_top_radius = radius_vec[0].parse().expect("Failed to parse radius");
        right_top_radius = left_top_radius;
        left_bottom_radius = radius_vec[1].parse().expect("Failed to parse radius");
        right_bottom_radius = left_bottom_radius;
    } else {
        left_top_radius = radius.parse().expect("Failed to parse radius");
        right_top_radius = left_top_radius;
        left_bottom_radius = left_top_radius;
        right_bottom_radius = left_top_radius;
    }

    let img = image::open(fpath).expect("Failed to open input image");
    let (width, height) = img.dimensions();
    let scale_factor = 4;
    let scaled_width = width * scale_factor;
    let scaled_height = height * scale_factor;

    let scaled_img = img.resize_exact(scaled_width, scaled_height, image::imageops::FilterType::Lanczos3);

    let mut output = ImageBuffer::new(scaled_width, scaled_height);

    for y in 0..scaled_height {
        for x in 0..scaled_width {
            let pixel = scaled_img.get_pixel(x, y);
            if is_inside_rounded_corner(x, y, scaled_width, scaled_height, &vec![left_top_radius * scale_factor, right_top_radius * scale_factor, left_bottom_radius * scale_factor, right_bottom_radius * scale_factor]) {
                output.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            } else {
                output.put_pixel(x, y, pixel);
            }
        }
    }

    let rounded_img = DynamicImage::ImageRgba8(output).resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    let input_path = Path::new(fpath);
    let mut output_path = PathBuf::from(input_path.parent().unwrap());
    let file_stem = input_path.file_stem().unwrap().to_str().unwrap();
    let extension = input_path.extension().unwrap().to_str().unwrap();
    let output_file_name = format!("{}_radius.{}", file_stem, extension);
    output_path.push(output_file_name);

    let output_path_str = output_path.to_str().unwrap().to_string();
    rounded_img.save(output_path).expect("Failed to save image");
    output_path_str
}

fn is_inside_rounded_corner(x: u32, y: u32, width: u32, height: u32, radius: &Vec<u32>) -> bool {
    let left_top_radius = radius[0];
    let right_top_radius = radius[1];
    let left_bottom_radius = radius[2];
    let right_bottom_radius = radius[3];
    if x < left_top_radius && y < left_top_radius {
        let dx = left_top_radius as i32 - x as i32;
        let dy = left_top_radius as i32 - y as i32;
        return dx * dx + dy * dy > (left_top_radius * left_top_radius) as i32;
    }

    if x >= width - right_top_radius && y < right_top_radius {
        let dx = x as i32 - (width - right_top_radius) as i32;
        let dy = right_top_radius as i32 - y as i32;
        return dx * dx + dy * dy > (right_top_radius * right_top_radius) as i32;
    }

    if x < left_bottom_radius && y >= height - left_bottom_radius {
        let dx = left_bottom_radius as i32 - x as i32;
        let dy = y as i32 - (height - left_bottom_radius) as i32;
        return dx * dx + dy * dy > (left_bottom_radius * left_bottom_radius) as i32;
    }

    if x >= width - right_bottom_radius && y >= height - right_bottom_radius {
        let dx = x as i32 - (width - right_bottom_radius) as i32;
        let dy = y as i32 - (height - right_bottom_radius) as i32;
        return dx * dx + dy * dy > (right_bottom_radius * right_bottom_radius) as i32;
    }
    false
}

pub fn resize_image_with_scaler(fpath: &str, out_path: Option<&str>, width: u32, height: u32) -> Option<DynamicImage> {
  // 验证尺寸不为零
  if width == 0 || height == 0 {
      eprintln!("警告: 尝试调整图像大小为 0x0，跳过处理: {}", fpath);
      return None;
  }

  let img = match ImageReader::open(fpath) {
      Ok(reader) => match reader.decode() {
          Ok(img) => img,
          Err(e) => {
              eprintln!("错误: 无法解码图像 {}: {:?}", fpath, e);
              return None;
          }
      },
      Err(e) => {
          eprintln!("错误: 无法打开图像文件 {}: {:?}", fpath, e);
          return None;
      }
  };

  let input_path = Path::new(fpath);

  let dimensions = img.dimensions();
  let mut bytes = Vec::from(img.as_bytes());
  let mut scaler = LinearScaler::new(ResamplingFunction::Lanczos3);
  scaler.set_threading_policy(ThreadingPolicy::Adaptive);

  let store = match ImageStore::<u8, 4>::from_slice(&mut bytes, dimensions.0 as usize, dimensions.1 as usize) {
      Ok(store) => store,
      Err(e) => {
          eprintln!("错误: 无法创建图像存储 {}: {:?}", fpath, e);
          return None;
      }
  };

  let resized = scaler.resize_rgba(
      ImageSize::new(width as usize, height as usize),
      store,
      true,
  );

  let binding = match resized {
      Ok(binding) => binding,
      Err(e) => {
          eprintln!("错误: 无法调整图像大小 {} 到 {}x{}: {:?}", fpath, width, height, e);
          return None;
      }
  };

  let resized_image = binding.as_bytes();
  let rgba = match ImageBuffer::from_raw(width, height, resized_image.to_vec()) {
      Some(rgba) => rgba,
      None => {
          eprintln!("错误: 无法从原始数据创建图像缓冲区 {}x{}", width, height);
          return None;
      }
  };

  let dynamic_image = DynamicImage::ImageRgba8(rgba);

  if let Some(out_path) = out_path {
      let mut output_path = PathBuf::from(input_path.parent().unwrap());
      output_path.push(out_path);
      if let Err(e) = dynamic_image.save(&output_path) {
          eprintln!("错误: 无法保存图像到 {}: {:?}", output_path.display(), e);
      }
      None
  } else {
      Some(dynamic_image)
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
        DynamicImage::ImageRgba8(output).save(&output_path).expect("Failed to save image");
    }
}

pub fn generate_chromium_ico(fpath: &str,out_path: &str) -> String{
    println!("Generating chromium ico {}", fpath);
    let input_path = Path::new(fpath);
    let output_path = PathBuf::from(input_path.parent().unwrap()).join(out_path);

    let sizes = vec![256, 128, 64, 48, 32, 24, 16];
    let mut icon_dir = IconDir::new(ico::ResourceType::Icon);

    for size in sizes {
        let save_path = PathBuf::from(input_path.parent().unwrap()).join(format!("{}_{}.png", input_path.file_stem().unwrap().to_str().unwrap(), size));
        resize_image_with_scaler(fpath, Some(save_path.to_str().unwrap()), size, size);
        let resized_img = image::open(save_path).expect("Failed to open resized image");
        let rgb = resized_img.to_rgba8();
        let icon_image = IconImage::from_rgba_data(size as u32, size as u32, rgb.to_vec());
        icon_dir.add_entry(ico::IconDirEntry::encode(&icon_image).unwrap());
    }

    let file = File::create(output_path.clone()).expect("Failed to create icon file");
    icon_dir.write(file).expect("Failed to write icon file");
    output_path.to_str().unwrap().to_string()
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

pub fn generate_chromium_icns(fpath: &str, out_path: &str, border: bool) -> String {
    println!("Generating chromium icns {}", fpath);
    let input_path = Path::new(fpath);
    let output_path = PathBuf::from(input_path.parent().unwrap()).join(out_path);
    
    let mut fix_path = input_path.to_path_buf();
    let tmp_file_path: Option<PathBuf> = if border {
        let img = image::open(fpath).expect("Failed to open image");
        let (width, height) = img.dimensions();
        // 确保 rate 至少为 1，避免 resize_size 为 0
        let rate = (width as f64 / 256.0).ceil() as u32;
        let resize_size = 208 * rate.max(1);
        if let Some(resized_img) = resize_image_with_scaler(fpath, None, resize_size, resize_size) {
            let mut img = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(width, height);
            let x = (width - resize_size) / 2;
            let y = (height - resize_size) / 2;
            image::imageops::overlay(&mut img, &resized_img, x as i64, y as i64);
            let tmp_path = PathBuf::from(input_path.parent().unwrap()).join("tmp_app.png");
            fix_path = tmp_path.clone();
            DynamicImage::ImageRgba8(img).save(&fix_path).expect("Failed to save image");
            Some(tmp_path)
        } else {
            eprintln!("警告: 无法调整图像大小，跳过边框处理");
            None
        }
    } else {
        None
    };

    let sizes = vec![512, 256, 128, 64, 32, 16];
    let mut icon_family = IconFamily::new();

    for size in sizes {
        let resized_img = resize_image_with_scaler(fix_path.to_str().unwrap(), None, size, size);
        if let Some(resized_img) = resized_img {
            let rgba32f = resized_img.to_rgba32f();
            let rgba: Vec<u8> = rgba32f.iter().map(|&f| (f * 255.0) as u8).collect();
            let icns_image = IcnsImage::from_data(icns::PixelFormat::RGBA, size as u32, size as u32, rgba).expect("Failed to create icns image");
            let icon_type = match size {
                512 => IconType::RGBA32_512x512,
                256 => IconType::RGBA32_256x256,
                128 => IconType::RGBA32_128x128,
                64 => IconType::RGBA32_64x64,
                32 => IconType::RGBA32_32x32,
                16 => IconType::RGBA32_16x16,
                _ => continue,
            };
            icon_family.add_icon_with_type(&icns_image, icon_type).expect("Failed to add icon");
        }
    }

    let file = File::create(&output_path).expect("Failed to create icns file");
    icon_family.write(file).expect("Failed to write icns file");
    
    // 清理临时文件
    if let Some(tmp_path) = tmp_file_path {
        if tmp_path.exists() {
            let _ = std::fs::remove_file(&tmp_path);
        }
    }
    
    output_path.to_str().unwrap().to_string()
}
 

#[allow(dead_code)]
pub fn generate_nine_patch_with_corners(fpath: &str, radius: &str) {
    let img = image::open(fpath).expect("Failed to open input image");
    let (width, height) = img.dimensions();
    let input_path = Path::new(fpath);
    let mut output_path = PathBuf::from(input_path.parent().unwrap());
    let file_stem = input_path.file_stem().unwrap().to_str().unwrap();
    let extension = input_path.extension().unwrap().to_str().unwrap();

    let (left_top_radius, right_top_radius, left_bottom_radius, right_bottom_radius) = if radius.contains(",") {
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
        ("top_center", left_top_radius, 0, width - left_top_radius - right_top_radius, left_top_radius),
        ("top_right", width - right_top_radius, 0, right_top_radius, right_top_radius),
        ("middle_left", 0, left_top_radius, left_top_radius, height - left_top_radius - left_bottom_radius),
        ("middle_center", left_top_radius, left_top_radius, width - left_top_radius - right_top_radius, height - left_top_radius - left_bottom_radius),
        ("middle_right", width - right_top_radius, left_top_radius, right_top_radius, height - right_top_radius - right_bottom_radius),
        ("bottom_left", 0, height - left_bottom_radius, left_bottom_radius, left_bottom_radius),
        ("bottom_center", left_bottom_radius, height - left_bottom_radius, width - left_bottom_radius - right_bottom_radius, left_bottom_radius),
        ("bottom_right", width - right_bottom_radius, height - right_bottom_radius, right_bottom_radius, right_bottom_radius),
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
    DynamicImage::ImageRgba8(output).save(&output_path).expect("Failed to save image");
}

pub fn generate_grayscale_image(fpath: &str, out_path: &str, size: u32) {
    if let Some(resized_img) = resize_image_with_scaler(fpath, None, size, size) {
        let grayscale_img = resized_img.to_luma_alpha8();
        let input_path = Path::new(fpath);
        let mut output_path = PathBuf::from(input_path.parent().unwrap());
        output_path.push(out_path);
        DynamicImage::ImageLumaA8(grayscale_img).save(&output_path).expect("Failed to save image");
    }
}