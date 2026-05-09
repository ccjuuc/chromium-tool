use axum::{
    extract::Path,
    extract::Json,
    http::header::CONTENT_TYPE,
    http::StatusCode,
    response::{Html, IntoResponse},
};
use std::env::{current_dir, var_os};
use std::path::PathBuf;
use serde::Serialize;
use base64::engine::general_purpose::STANDARD;
use base64::engine::Engine;
use crate::model::oem::{ConvertRequest, OemRequest, CornerRequest};
use crate::image::{chromium_icon, image_util, svg_png};
use crate::service::oem::ThemeGenerator;

pub async fn oem_page() -> impl IntoResponse {
    let html_content = include_str!("../../templates/index.html");
    Html(html_content.to_string())
}

/// 格式转换的输入/输出目录。
///
/// - 默认：`{进程当前工作目录}/convert_output/`（与 OEM 的 `oem_logo/` 类似，避免把文件散在 CWD 根目录）
/// - 可设置环境变量 `CHROMIUM_TOOL_CONVERT_DIR` 为绝对路径，强制输出到指定文件夹（例如 `H:\chromium-tool\out`）
///
/// 说明：若你看到文件在 `C:\...`，是因为**启动服务时 shell 的当前目录在 C 盘**（例如在
/// `C:\Users\...\AppData\Local\Temp` 下运行了 exe），不是程序“写死 C 盘”。
fn convert_work_dir() -> Result<PathBuf, std::io::Error> {
    if let Some(raw) = var_os("CHROMIUM_TOOL_CONVERT_DIR") {
        let p = PathBuf::from(raw);
        if !p.as_os_str().is_empty() {
            return Ok(p);
        }
    }
    Ok(current_dir()?.join("convert_output"))
}

/// 仅允许单层文件名，禁止路径穿越。
fn sanitize_convert_output_basename(raw: &str) -> Result<String, &'static str> {
    let name = std::path::Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid output path")?;
    let mut it = std::path::Path::new(name).components();
    match (it.next(), it.next()) {
        (Some(std::path::Component::Normal(_)), None) => {}
        _ => return Err("invalid output filename"),
    }
    if name.is_empty() || name.len() > 512 {
        return Err("invalid output filename");
    }
    Ok(name.to_string())
}

/// 把上传文件名转成 `<stem>-ori<ext>`，与转换输出名隔离，避免互相覆盖。
///
/// - `foo.svg`        → `foo-ori.svg`
/// - `foo`            → `foo-ori`
/// - `archive.tar.gz` → `archive.tar-ori.gz`（按最后一个 `.` 切）
/// - 已经带 `-ori` 后缀的不会再加一遍。
fn original_storage_name(raw: &str) -> String {
    let p = std::path::Path::new(raw);
    let file_name = p
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(raw);
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            if stem.ends_with("-ori") {
                file_name.to_string()
            } else {
                format!("{}-ori.{}", stem, ext)
            }
        }
        _ => {
            if file_name.ends_with("-ori") {
                file_name.to_string()
            } else {
                format!("{}-ori", file_name)
            }
        }
    }
}

fn convert_output_content_type(file_name: &str) -> &'static str {
    match std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("icns") => "application/octet-stream",
        Some("icon") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// 供前端预览：从 `convert_work_dir` 读取刚转换的文件（仅单层 basename）。
pub async fn get_convert_output(Path(file_name): Path<String>) -> impl IntoResponse {
    let safe = match sanitize_convert_output_basename(&file_name) {
        Ok(s) => s,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let work_dir = match convert_work_dir() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("convert dir: {}", e),
            )
                .into_response();
        }
    };
    let path = work_dir.join(&safe);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let ct = convert_output_content_type(&safe);
            (
                StatusCode::OK,
                [(CONTENT_TYPE, ct)],
                bytes,
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!("file not found: {} ({})", safe, e),
        )
            .into_response(),
    }
}

/// 将 `.icon` 转为 SVG，供浏览器 `<img>` 预览（原始 `/convert_output/*.icon` 为纯文本，不能直接作为图片）。
pub async fn get_convert_output_svg(Path(file_name): Path<String>) -> impl IntoResponse {
    let safe = match sanitize_convert_output_basename(&file_name) {
        Ok(s) => s,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    if !safe.to_ascii_lowercase().ends_with(".icon") {
        return (
            StatusCode::BAD_REQUEST,
            "SVG preview is only available for .icon files",
        )
            .into_response();
    }
    let work_dir = match convert_work_dir() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("convert dir: {}", e),
            )
                .into_response();
        }
    };
    let path = work_dir.join(&safe);
    let path_str = match path.to_str() {
        Some(s) => s,
        None => return (StatusCode::BAD_REQUEST, "Invalid path").into_response(),
    };
    match chromium_icon::try_convert_chromium_icon_path_to_svg_markup(path_str) {
        Ok(svg) => (
            StatusCode::OK,
            [(
                CONTENT_TYPE,
                "image/svg+xml; charset=utf-8",
            )],
            svg.into_bytes(),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

#[derive(Serialize)]
struct ConvertImageOk {
    message: String,
    preview_name: String,
}

pub async fn convert_image(Json(payload): Json<ConvertRequest>) -> impl IntoResponse {
    let work_dir = match convert_work_dir() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to resolve convert output directory: {}", e),
            )
                .into_response();
        }
    };

    if let Err(e) = std::fs::create_dir_all(&work_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create convert output directory {}: {}", work_dir.display(), e),
        )
            .into_response();
    }

    // 上传的原图统一以 `<stem>-ori<ext>` 落盘，与转换输出区分，避免互相覆盖、便于回溯。
    let original_name = original_storage_name(&payload.logo_name);
    let logo_path_buf = work_dir.join(&original_name);

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

    let safe_output_name = match sanitize_convert_output_basename(&payload.output_path) {
        Ok(s) => s,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let format = &payload.format;

    tracing::info!(
        "convert_image: original='{}', output='{}', format='{}', size={}",
        original_name,
        safe_output_name,
        format,
        logo_data.len()
    );

    // 把可能 panic 的转换函数放到 catch_unwind 里，确保即便底层 svg 解析
    // 等地方 panic，也能把可读的错误回给前端而不是返回空 500。
    let logo_path_owned = logo_path.to_string();
    let output_path_owned = safe_output_name.clone();
    let format_owned = format.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        match format_owned.as_str() {
            "ICO" => Ok(image_util::generate_chromium_ico(
                &logo_path_owned,
                &output_path_owned,
            )),
            "ICON" => chromium_icon::try_convert_svg_to_chromium_icon(
                &logo_path_owned,
                &output_path_owned,
            ),
            "ICNS" => Ok(image_util::generate_chromium_icns(
                &logo_path_owned,
                &output_path_owned,
                true,
            )),
            "PNG" => {
                if logo_path_owned.ends_with(".svg") {
                    Ok(svg_png::convert_svg_to_png(&logo_path_owned, &output_path_owned))
                } else {
                    Err("svg file is required for PNG conversion".to_string())
                }
            }
            "SVG" => {
                let lower = logo_path_owned.to_ascii_lowercase();
                if !lower.ends_with(".icon") {
                    return Err(".icon source file is required for SVG conversion".to_string());
                }
                let svg = chromium_icon::try_convert_chromium_icon_path_to_svg_markup(&logo_path_owned)?;
                let parent = std::path::Path::new(&logo_path_owned)
                    .parent()
                    .ok_or_else(|| "invalid logo path".to_string())?;
                let out_full = parent.join(&output_path_owned);
                std::fs::write(&out_full, svg.as_bytes()).map_err(|e| {
                    format!(
                        "Failed to write SVG to {}: {}",
                        out_full.display(),
                        e
                    )
                })?;
                Ok(out_full.to_string_lossy().into_owned())
            }
            other => Err(format!("Unsupported format: {}", other)),
        }
    }));

    match result {
        Ok(Ok(ret)) => {
            tracing::info!("convert_image ok: {}", ret);
            (
                StatusCode::OK,
                Json(ConvertImageOk {
                    message: ret,
                    preview_name: safe_output_name,
                }),
            )
                .into_response()
        }
        Ok(Err(msg)) => {
            tracing::error!("convert_image error: {}", msg);
            (StatusCode::BAD_REQUEST, msg).into_response()
        }
        Err(panic_payload) => {
            // panic 信息可能是 &str 或 String
            let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "panic without message".to_string()
            };
            tracing::error!("convert_image panicked: {}", msg);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Conversion panicked: {}", msg),
            )
                .into_response()
        }
    }
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

#[cfg(test)]
mod tests {
    use super::original_storage_name;

    #[test]
    fn appends_ori_before_extension() {
        assert_eq!(original_storage_name("envelope.svg"), "envelope-ori.svg");
        assert_eq!(original_storage_name("foo.bar.png"), "foo.bar-ori.png");
        assert_eq!(original_storage_name("a.b.c.icon"), "a.b.c-ori.icon");
    }

    #[test]
    fn appends_ori_when_no_extension() {
        assert_eq!(original_storage_name("Makefile"), "Makefile-ori");
    }

    #[test]
    fn handles_dotfiles_without_stem() {
        // 没有 stem 时（".env"），不能切成 "-ori.env"，按"无扩展名"处理。
        assert_eq!(original_storage_name(".env"), ".env-ori");
    }

    #[test]
    fn idempotent_when_already_ori() {
        assert_eq!(original_storage_name("envelope-ori.svg"), "envelope-ori.svg");
        assert_eq!(original_storage_name("Makefile-ori"), "Makefile-ori");
    }

    #[test]
    fn strips_directory_components() {
        // 防御：即使前端塞了相对路径，也只取文件名部分。
        assert_eq!(original_storage_name("sub/dir/foo.svg"), "foo-ori.svg");
        assert_eq!(original_storage_name("..\\bar.png"), "bar-ori.png");
    }
}
