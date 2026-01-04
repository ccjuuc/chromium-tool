use axum::{
    extract::Json,
    http::StatusCode,
    response::{Html, IntoResponse},
};
use std::env::current_dir;
use base64::engine::general_purpose::STANDARD;
use base64::engine::Engine;
use crate::model::oem::{ConvertRequest, OemRequest, CornerRequest};
use crate::image::{chromium_icon, image_util, svg_png};
use crate::service::oem::ThemeGenerator;

pub async fn oem_page() -> impl IntoResponse {
    let html_content = include_str!("../../templates/index.html");
    Html(html_content.to_string())
}

pub async fn convert_image(Json(payload): Json<ConvertRequest>) -> impl IntoResponse {
    
    let logo_path_buf = match current_dir() {
        Ok(dir) => dir.join(&payload.logo_name),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get current dir: {}", e)).into_response(),
    };
    
    let logo_path = match logo_path_buf.to_str() {
        Some(path) => path,
        None => return (StatusCode::BAD_REQUEST, "Invalid logo path").into_response(),
    };
    
    let logo_data = match STANDARD.decode(&payload.logo_data) {
        Ok(data) => data,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid base64 data: {}", e)).into_response(),
    };
    
    if let Err(e) = std::fs::write(logo_path, &logo_data) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write file: {}", e)).into_response();
    }
    
    let output_path = &payload.output_path;
    let format = &payload.format;
    
    let ret = match format.as_str() {
        "ICO" => image_util::generate_chromium_ico(logo_path, output_path),
        "ICON" => chromium_icon::convert_svg_to_chromium_icon(logo_path, output_path),
        "ICNS" => image_util::generate_chromium_icns(logo_path, output_path, true),
        "PNG" => {
            if logo_path.ends_with(".svg") {
                svg_png::convert_svg_to_png(logo_path, output_path)
            } else {
                return (StatusCode::BAD_REQUEST, "svg file is required for PNG conversion").into_response();
            }
        }
        _ => return (StatusCode::BAD_REQUEST, "Unsupported format").into_response(),
    };
    
    (StatusCode::OK, ret).into_response()
}

pub async fn oem_convert(Json(payload): Json<OemRequest>) -> impl IntoResponse {
    
    let logo_dir = match current_dir() {
        Ok(dir) => dir.join("oem_logo"),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get current dir: {}", e)).into_response(),
    };
    
    if !logo_dir.exists() {
        if let Err(e) = std::fs::create_dir(&logo_dir) {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create logo dir: {}", e)).into_response();
        }
    }
    
    // 准备主题输出目录
    let theme_dir = match current_dir() {
        Ok(dir) => dir.join("oem").join("theme"),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get current dir: {}", e)).into_response(),
    };
    
    let logo_path = if !payload.logo_name.is_empty() && !payload.logo_data.is_empty() {
        let logo_path_buf = logo_dir.join(&payload.logo_name);
        let logo_path_str = match logo_path_buf.to_str() {
            Some(path) => path,
            None => return (StatusCode::BAD_REQUEST, "Invalid logo path").into_response(),
        };
        
        let logo_data = match STANDARD.decode(&payload.logo_data) {
            Ok(data) => data,
            Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid base64 data: {}", e)).into_response(),
        };
        
        if let Err(e) = std::fs::write(logo_path_str, &logo_data) {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write logo: {}", e)).into_response();
        }
        
        let format = payload.logo_name.split('.').last().unwrap_or("png");
        
        let mut fix_logo_path = std::path::PathBuf::from(logo_path_str);
        if format == "svg" {
            fix_logo_path.set_file_name("tmp.png");
            if let Some(name) = fix_logo_path.file_name().and_then(|n| n.to_str()) {
                svg_png::convert_svg_to_png(logo_path_str, name);
            }
            chromium_icon::convert_svg_to_chromium_icon(logo_path_str, "product.icon");
        }
        
        fix_logo_path.to_str().map(|s| s.to_string())
    } else {
        None
    };
    
    let document_path = if !payload.document_name.is_empty() && !payload.document_data.is_empty() {
        let document_path_buf = logo_dir.join(&payload.document_name);
        let document_path_str = match document_path_buf.to_str() {
            Some(path) => path,
            None => return (StatusCode::BAD_REQUEST, "Invalid document path").into_response(),
        };
        
        let document_data = match STANDARD.decode(&payload.document_data) {
            Ok(data) => data,
            Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid base64 data: {}", e)).into_response(),
        };
        
        if let Err(e) = std::fs::write(document_path_str, &document_data) {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write document: {}", e)).into_response();
        }
        
        Some(document_path_str.to_string())
    } else {
        None
    };
    
    // 使用主题生成器生成所有资源
    if let Some(logo) = logo_path.as_ref() {
        let oem_name = payload.logo_name.split('.').next().unwrap_or("oem");
        let generator = ThemeGenerator::new(&theme_dir, oem_name);
        
        match generator.generate_all(logo, document_path.as_deref()).await {
            Ok(_) => (StatusCode::OK, format!("OEM theme resources created successfully in oem/theme/")).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate theme resources: {}", e)).into_response(),
        }
    } else {
        (StatusCode::BAD_REQUEST, "Logo is required").into_response()
    }
}

pub async fn add_rounded_corners(Json(payload): Json<CornerRequest>) -> impl IntoResponse {
    
    let logo_path_buf = match current_dir() {
        Ok(dir) => dir.join(&payload.logo_name),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get current dir: {}", e)).into_response(),
    };
    
    let logo_path = match logo_path_buf.to_str() {
        Some(path) => path,
        None => return (StatusCode::BAD_REQUEST, "Invalid logo path").into_response(),
    };
    
    let logo_data = match STANDARD.decode(&payload.logo_data) {
        Ok(data) => data,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid base64 data: {}", e)).into_response(),
    };
    
    if let Err(e) = std::fs::write(logo_path, &logo_data) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write file: {}", e)).into_response();
    }
    
    let radius = &payload.radius;
    let outpath = image_util::apply_rounded_corners(logo_path, radius);
    (StatusCode::OK, outpath).into_response()
}

