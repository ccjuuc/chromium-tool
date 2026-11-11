use axum::{
    extract::{Json, Path, Query},
    http::header::CONTENT_TYPE,
    http::StatusCode,
    response::{Html, IntoResponse},
};
use std::env::{current_dir, var_os};
use std::path::PathBuf;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock, RwLock, atomic::{AtomicU64, Ordering}};
use serde::{Deserialize, Serialize};
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
    if raw.contains(['/', '\\', ':', '<', '>', '"', '|', '?', '*']) || raw.ends_with([' ', '.']) {
        return Err("output must be a single filename");
    }
    let name = std::path::Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("invalid output path")?;
    let mut it = std::path::Path::new(name).components();
    match (it.next(), it.next()) {
        (Some(std::path::Component::Normal(_)), None) => {}
        _ => return Err("invalid output filename"),
    }
    if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err("invalid output filename");
    }
    let device = name.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    if matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || device.strip_prefix("COM").or_else(|| device.strip_prefix("LPT"))
            .is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")) {
        return Err("reserved output filename");
    }
    Ok(name.to_string())
}

#[derive(Deserialize, Default)]
pub struct IconPreviewRootQuery {
    #[serde(default)]
    root: Option<String>,
    /// 预览衬底：`light`（默认白）或 `dark`（黑）。影响 `.icon`→SVG 时写入的 canvas 矩形。
    #[serde(default)]
    bg: Option<String>,
}

fn registered_preview_directories() -> &'static RwLock<HashSet<PathBuf>> {
    static DIRECTORIES: OnceLock<RwLock<HashSet<PathBuf>>> = OnceLock::new();
    DIRECTORIES.get_or_init(|| RwLock::new(HashSet::new()))
}

fn canonical_directory(path: &std::path::Path) -> Result<PathBuf, String> {
    let path = path.canonicalize().map_err(|e| format!("无法访问目录: {} ({e})", path.display()))?;
    if !path.is_dir() { return Err("指定路径必须是目录".into()); }
    Ok(path)
}

fn register_preview_directory(path: &std::path::Path) -> Result<PathBuf, String> {
    let path = canonical_directory(path)?;
    registered_preview_directories().write().map_err(|_| "目录登记不可用")?.insert(path.clone());
    Ok(path)
}

/// Only the default output directory and directories explicitly selected or
/// submitted for conversion through the same-origin UI may be read.
fn resolve_icon_preview_directory(root_param: Option<&str>) -> Result<PathBuf, String> {
    let default = convert_work_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&default).map_err(|e| e.to_string())?;
    let default = canonical_directory(&default)?;
    let requested = match root_param.map(str::trim).filter(|s| !s.is_empty()) {
        Some(path) => canonical_directory(std::path::Path::new(path))?,
        None => return Ok(default),
    };
    if requested == default || registered_preview_directories().read()
        .map_err(|_| "目录登记不可用")?.contains(&requested) {
        Ok(requested)
    } else {
        Err("该目录尚未授权，请先选择目录或执行转换".into())
    }
}

fn is_preview_asset(name: &str) -> bool {
    matches!(std::path::Path::new(name).extension().and_then(|v| v.to_str())
        .map(str::to_ascii_lowercase).as_deref(),
        Some("svg" | "icon" | "png" | "ico" | "icns" | "jpg" | "jpeg" | "webp" | "bmp"))
}

fn resolve_preview_asset(root: &std::path::Path, name: &str) -> Result<PathBuf, String> {
    let name = sanitize_convert_output_basename(name).map_err(str::to_string)?;
    if !is_preview_asset(&name) { return Err("仅允许读取图像或图标文件".into()); }
    let path = root.join(name).canonicalize().map_err(|e| format!("文件不可用: {e}"))?;
    if path.parent() != Some(root) || !path.is_file() {
        return Err("文件不在授权目录内".into());
    }
    Ok(path)
}

fn checked_output_path(root: &std::path::Path, name: &str) -> Result<PathBuf, String> {
    let name = sanitize_convert_output_basename(name).map_err(str::to_string)?;
    let path = root.join(name);
    if std::fs::symlink_metadata(&path).is_ok() {
        let existing = path.canonicalize().map_err(|e| e.to_string())?;
        if existing.parent() != Some(root) || !existing.is_file() {
            return Err("输出文件不在工作目录内".into());
        }
    }
    Ok(path)
}

/// 将 Windows `canonicalize` 产生的 verbatim 路径转换为适合界面展示和再次输入的常规路径。
fn display_icon_preview_directory(path: &std::path::Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = value.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        value.into_owned()
    }
}

/// 把上传文件名转成 `<stem>-ori<ext>`，与转换输出名隔离，避免互相覆盖。
///
/// - `foo.svg`        → `foo-ori.svg`
/// - `foo`            → `foo-ori`
/// - `archive.tar.gz` → `archive.tar-ori.gz`（按最后一个 `.` 切）
/// - 已经带 `-ori` 后缀的不会再加一遍。
fn original_storage_name(raw: &str) -> String {
    // 同时兼容 `/` 与 Windows `\` 分隔符（macOS/Linux 上 `Path` 不把 `\` 视为分隔符）。
    let file_name = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
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
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// 供前端预览：从 `convert_work_dir`（或查询参数 `root` 指定目录）读取文件（仅单层 basename）。
pub async fn get_convert_output(
    Path(file_name): Path<String>,
    Query(q): Query<IconPreviewRootQuery>,
) -> impl IntoResponse {
    let safe = match sanitize_convert_output_basename(&file_name) {
        Ok(s) => s,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let work_dir = match resolve_icon_preview_directory(q.root.as_deref()) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let path = match resolve_preview_asset(&work_dir, &safe) {
        Ok(path) => path,
        Err(message) => return (StatusCode::FORBIDDEN, message).into_response(),
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let ct = convert_output_content_type(&safe);
            (
                StatusCode::OK,
                [(CONTENT_TYPE.as_str(), ct), ("x-content-type-options", "nosniff"),
                 ("content-security-policy", "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:")],
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
///
/// 可选查询参数 `root`：与 [`list_icon_preview_files`] 一致，指定 `.icon` 所在目录；
/// 省略时仍为 [`convert_work_dir`]。
pub async fn get_convert_output_svg(
    Path(file_name): Path<String>,
    Query(q): Query<IconPreviewRootQuery>,
) -> impl IntoResponse {
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
    let work_dir = match resolve_icon_preview_directory(q.root.as_deref()) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let path = match resolve_preview_asset(&work_dir, &safe) {
        Ok(path) => path,
        Err(message) => return (StatusCode::FORBIDDEN, message).into_response(),
    };
    let path_str = match path.to_str() {
        Some(s) => s,
        None => return (StatusCode::BAD_REQUEST, "Invalid path").into_response(),
    };
    match chromium_icon::try_convert_chromium_icon_path_to_svg_markup_with_backdrop(
        path_str,
        q.bg.as_deref(),
    ) {
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

/// 批量 .icon 预览页（列出 `convert_output` 目录下所有 `.icon`，纵向展示，边长可调）。
pub async fn icon_batch_preview_page() -> impl IntoResponse {
    Html(include_str!("../../templates/icon_batch_preview.html").to_string())
}

#[derive(Deserialize, Default)]
pub struct IconPreviewSelectDirectoryRequest {
    #[serde(default)]
    initial_directory: Option<String>,
}

#[derive(Serialize)]
struct IconPreviewSelectDirectoryResponse {
    directory: Option<String>,
}

/// 打开系统原生目录选择器。此工具作为本机服务运行，所选绝对路径可直接供预览与转换接口使用。
pub async fn select_icon_preview_directory(
    Json(body): Json<IconPreviewSelectDirectoryRequest>,
) -> impl IntoResponse {
    let initial = body
        .initial_directory
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let selected = match super::directory_picker::pick_directory(initial).await {
        Ok(path) => path,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("目录选择器启动失败: {}", e),
            )
                .into_response();
        }
    };

    let directory = match selected.as_deref().map(register_preview_directory).transpose() {
        Ok(directory) => directory,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    Json(IconPreviewSelectDirectoryResponse {
        directory: directory.as_deref().map(display_icon_preview_directory),
    })
    .into_response()
}

#[derive(Serialize)]
struct IconPreviewList {
    icons: Vec<String>,
    originals: std::collections::HashMap<String, String>,
    directory: String,
}

/// 返回给定根目录（或默认 `convert_work_dir`）下所有 `.icon` 文件名（已排序）及同目录匹配的原始 `.svg`，供批量预览页加载。
///
/// 查询参数：`root`，可选；见 [`resolve_icon_preview_directory`]。
pub async fn list_icon_preview_files(Query(q): Query<IconPreviewRootQuery>) -> impl IntoResponse {
    let work_dir = match resolve_icon_preview_directory(q.root.as_deref()) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let dir_display = display_icon_preview_directory(&work_dir);
    let mut icons: Vec<String> = Vec::new();
    let mut svg_by_lower_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let read_dir = match std::fs::read_dir(&work_dir) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read dir {}: {}", work_dir.display(), e),
            )
                .into_response();
        }
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        // 与下载/预览接口共用同一套 basename 校验，避免列出无法访问的奇怪名字。
        if resolve_preview_asset(&work_dir, &name).is_err() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".icon") {
            icons.push(name);
        } else if lower.ends_with(".svg") {
            svg_by_lower_name.insert(lower, name);
        }
    }
    icons.sort();
    let mut originals: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for icon_name in &icons {
        let stem = &icon_name[..icon_name.len().saturating_sub(5)];
        let candidate_direct = format!("{}.svg", stem).to_ascii_lowercase();
        let candidate_ori = format!("{}-ori.svg", stem).to_ascii_lowercase();
        if let Some(svg_name) = svg_by_lower_name
            .get(&candidate_direct)
            .or_else(|| svg_by_lower_name.get(&candidate_ori))
        {
            originals.insert(icon_name.clone(), svg_name.clone());
        }
    }
    Json(IconPreviewList {
        icons,
        originals,
        directory: dir_display,
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct IconPreviewBatchConvertRequest {
    /// 用户指定的目录（必填）。仅在此路径下列举并写入 `.icon`，不依赖 `CHROMIUM_TOOL_CONVERT_DIR`。
    pub directory: String,
    /// 是否在 `.icon` 中输出 `PATH_COLOR_ARGB`。省略或 `null` 视为 `true`。
    #[serde(default)]
    pub emit_path_colors: Option<bool>,
    #[serde(default)]
    pub allow_approximation: bool,
    /// 省略时转换全部 SVG；指定时只转换这些单层 SVG 文件名。
    #[serde(default)]
    pub files: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct IconPreviewBatchConvertItemError {
    pub file: String,
    pub code: &'static str,
    pub message: String,
}

impl IconPreviewBatchConvertItemError {
    fn new(file: String, error: chromium_icon::IconConversionError) -> Self {
        Self { file, code: error.kind.as_str(), message: error.message }
    }
}

#[derive(Serialize)]
pub struct IconPreviewBatchConvertResponse {
    /// canonical 后的目录，供预览接口 `root` 使用
    pub directory: String,
    pub svg_found: usize,
    pub converted: Vec<String>,
    pub errors: Vec<IconPreviewBatchConvertItemError>,
    pub summary: String,
}

/// 将用户指定目录下全部或选定的 `*.svg` 转为同目录下的 `同名.icon`。
///
/// 目录必须显式传入；**不会**用环境变量代替用户输入。转换完成后由前端再请求 [`list_icon_preview_files`]。
pub async fn icon_preview_batch_convert(
    Json(body): Json<IconPreviewBatchConvertRequest>,
) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || convert_icon_directory(body)).await {
        Ok(response) => response,
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("转换任务失败: {error}")).into_response(),
    }
}

fn convert_icon_directory(body: IconPreviewBatchConvertRequest) -> axum::response::Response {
    let emit_path_colors = body.emit_path_colors.unwrap_or(true);
    let icon_opts = chromium_icon::SvgToChromiumIconOptions {
        emit_path_colors,
        approximate_effects: body.allow_approximation,
    };
    let trimmed = body.directory.trim();
    if trimmed.is_empty() {
        return (StatusCode::BAD_REQUEST, "请填写目录路径").into_response();
    }
    let root = match register_preview_directory(std::path::Path::new(trimmed)) {
        Ok(p) => p,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    let dir_str = display_icon_preview_directory(&root);

    let svg_files = match batch_svg_files(&root, body.files.as_deref()) {
        Ok(files) => files,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };

    let svg_found = svg_files.len();
    let mut converted: Vec<String> = Vec::new();
    let mut errors: Vec<IconPreviewBatchConvertItemError> = Vec::new();

    for path in svg_files {
        let fname = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => {
                errors.push(IconPreviewBatchConvertItemError::new(fname, "无法解析文件名".into()));
                continue;
            }
        };
        let out_basename = format!("{}.icon", stem);
        if let Err(message) = resolve_preview_asset(&root, &fname)
            .and_then(|_| checked_output_path(&root, &out_basename)) {
            errors.push(IconPreviewBatchConvertItemError::new(fname, message.into()));
            continue;
        }
        let svg_path_str = match path.to_str() {
            Some(s) => s.to_string(),
            None => {
                errors.push(IconPreviewBatchConvertItemError::new(fname, "路径含非法字符".into()));
                continue;
            }
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            chromium_icon::try_convert_svg_to_chromium_icon_detailed(
                &svg_path_str,
                &out_basename,
                &icon_opts,
            )
        }));
        match result {
            Ok(Ok(_)) => converted.push(out_basename),
            Ok(Err(error)) => errors.push(IconPreviewBatchConvertItemError::new(fname, error)),
            Err(_) => errors.push(IconPreviewBatchConvertItemError::new(fname, "转换过程 panic".into())),
        }
    }

    let requires_approximation = errors.iter().filter(|error|
        error.code == chromium_icon::IconConversionErrorKind::RequiresApproximation.as_str()
    ).count();
    let summary = format!(
        "本次处理 SVG {} 个，成功 {} 个，需要近似转换 {} 个，失败 {} 个",
        svg_found,
        converted.len(),
        requires_approximation,
        errors.len() - requires_approximation
    );

    Json(IconPreviewBatchConvertResponse {
        directory: dir_str,
        svg_found,
        converted,
        errors,
        summary,
    })
    .into_response()
}

fn batch_svg_files(root: &std::path::Path, selected: Option<&[String]>) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if let Some(selected) = selected {
        if selected.is_empty() {
            return Err("请选择需要转换的 SVG 文件".into());
        }
        // Validate the entire selection before writing anything. An invalid
        // basename must not turn a targeted retry into a directory-wide run.
        for name in selected {
            let name = sanitize_convert_output_basename(name).map_err(str::to_string)?;
            if !std::path::Path::new(&name).extension().and_then(|value| value.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")) {
                return Err("仅能选择 SVG 源文件".into());
            }
            resolve_preview_asset(root, &name)?;
            files.push(root.join(name));
        }
    } else {
        for entry in std::fs::read_dir(root).map_err(|error| format!("无法读取目录: {error}"))? {
            let path = entry.map_err(|error| format!("无法读取目录项: {error}"))?.path();
            if path.is_file() && path.extension().and_then(|value| value.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")) {
                files.push(path);
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

#[derive(Serialize)]
struct ConvertImageOk {
    message: String,
    preview_name: String,
}

#[derive(Serialize)]
struct ConvertImageError {
    code: &'static str,
    message: String,
}

impl From<chromium_icon::IconConversionError> for ConvertImageError {
    fn from(error: chromium_icon::IconConversionError) -> Self {
        Self { code: error.kind.as_str(), message: error.message }
    }
}

fn validate_conversion_request(payload: &ConvertRequest) -> Result<(String, String, String), String> {
    let output_name = sanitize_convert_output_basename(&payload.output_path).map_err(str::to_string)?;
    let original_name = sanitize_convert_output_basename(&original_storage_name(&payload.logo_name))
        .map_err(str::to_string)?;
    let format = payload.format.to_ascii_uppercase();
    let expected_extension = match format.as_str() {
        "ICON" => "icon", "ICO" => "ico", "ICNS" => "icns", "PNG" => "png", "SVG" => "svg",
        _ => return Err(format!("Unsupported format: {}", payload.format)),
    };
    let output_extension = std::path::Path::new(&output_name).extension().and_then(|s| s.to_str()).unwrap_or("");
    if !output_extension.eq_ignore_ascii_case(expected_extension) {
        return Err("输出文件后缀与转换格式不一致".into());
    }
    if original_name.eq_ignore_ascii_case(&output_name) {
        return Err("输出文件不能覆盖上传的原图".into());
    }
    let source_extension = std::path::Path::new(&original_name).extension()
        .and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    match format.as_str() {
        "ICON" | "PNG" if source_extension != "svg" => return Err("此转换需要 SVG 源文件".into()),
        "SVG" if source_extension != "icon" => return Err("SVG 导出需要 .icon 源文件".into()),
        "ICO" | "ICNS" if !matches!(source_extension.as_str(), "svg" | "png" | "jpg" | "jpeg" | "webp" | "bmp" | "ico" | "tif" | "tiff") => {
            return Err("不支持此源图像格式".into());
        }
        _ => {}
    }
    Ok((original_name, output_name, format))
}

struct ConversionStaging(PathBuf);

impl ConversionStaging {
    fn new(root: &std::path::Path) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!(".convert-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("无法创建转换目录: {error}")),
            }
        }
        Err("无法创建独立转换目录".into())
    }
}

impl Drop for ConversionStaging {
    fn drop(&mut self) {
        // This exact directory was exclusively created by ConversionStaging::new.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_image_conversion(payload: ConvertRequest) -> Result<ConvertImageOk, chromium_icon::IconConversionError> {
    let work_dir = convert_work_dir().map_err(|e| e.to_string())?;
    run_image_conversion_in_directory(payload, &work_dir)
}

fn publish_conversion(
    staging: &ConversionStaging,
    input: &std::path::Path,
    original_destination: &std::path::Path,
    output: &std::path::Path,
    output_destination: &std::path::Path,
) -> Result<(), String> {
    // Publishing the source and result is serialized so concurrent requests
    // cannot pair one request's source with another request's result.
    static PUBLISH_LOCK: Mutex<()> = Mutex::new(());
    let _guard = PUBLISH_LOCK.lock().map_err(|_| "转换输出不可用")?;
    let previous_source = staging.0.join(".previous-source");
    let had_source = original_destination.exists();
    if had_source {
        std::fs::copy(original_destination, &previous_source)
            .map_err(|e| format!("无法备份原图: {e}"))?;
    }
    std::fs::rename(input, original_destination).map_err(|e| format!("无法保存原图: {e}"))?;
    if let Err(error) = std::fs::rename(output, output_destination) {
        let rollback = if had_source {
            std::fs::rename(previous_source, original_destination)
        } else {
            std::fs::remove_file(original_destination)
        };
        return match rollback {
            Ok(()) => Err(format!("无法保存输出，已恢复原图: {error}")),
            Err(rollback_error) => Err(format!("无法保存输出: {error}；恢复原图失败: {rollback_error}")),
        };
    }
    Ok(())
}

fn run_image_conversion_in_directory(
    payload: ConvertRequest,
    work_dir: &std::path::Path,
) -> Result<ConvertImageOk, chromium_icon::IconConversionError> {
    // Validate every name/format before creating or replacing any output.
    let (original_name, output_name, format) = validate_conversion_request(&payload)?;
    let logo_data = STANDARD.decode(&payload.logo_data).map_err(|e| format!("Invalid base64 data: {e}"))?;
    if logo_data.is_empty() { return Err("源文件不能为空".into()); }
    std::fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let work_dir = canonical_directory(work_dir)?;
    let original_destination = checked_output_path(&work_dir, &original_name)?;
    let output_destination = checked_output_path(&work_dir, &output_name)?;
    let staging = ConversionStaging::new(&work_dir)?;
    let input = staging.0.join(&original_name);
    std::fs::write(&input, logo_data).map_err(|e| format!("无法保存转换输入: {e}"))?;
    let input_str = input.to_str().ok_or("Invalid source path")?;
    let icon_options = chromium_icon::SvgToChromiumIconOptions {
        emit_path_colors: payload.emit_path_colors.unwrap_or(true),
        approximate_effects: payload.allow_approximation,
    };
    match format.as_str() {
        "ICO" => { image_util::try_generate_chromium_ico(input_str, &output_name)?; }
        "ICNS" => { image_util::try_generate_chromium_icns(input_str, &output_name, true)?; }
        "ICON" => { chromium_icon::try_convert_svg_to_chromium_icon_detailed(input_str, &output_name, &icon_options)?; }
        "PNG" => { svg_png::try_convert_svg_to_png(input_str, &output_name)?; }
        "SVG" => {
            let svg = chromium_icon::try_convert_chromium_icon_path_to_svg_markup(input_str)?;
            std::fs::write(staging.0.join(&output_name), svg).map_err(|e| format!("无法保存 SVG: {e}"))?;
        }
        _ => unreachable!("validated output format"),
    }
    let output = staging.0.join(&output_name);
    if std::fs::metadata(&output).map_err(|e| e.to_string())?.len() == 0 {
        return Err("转换未生成有效内容".into());
    }
    // Only completed conversions replace existing artifacts; failed conversions
    // leave both the previous output and its associated source untouched.
    publish_conversion(&staging, &input, &original_destination, &output, &output_destination)?;
    Ok(ConvertImageOk {
        message: output_destination.to_string_lossy().into_owned(),
        preview_name: output_name,
    })
}

pub async fn convert_image(Json(payload): Json<ConvertRequest>) -> impl IntoResponse {
    match tokio::task::spawn_blocking(move || run_image_conversion(payload)).await {
        Ok(Ok(result)) => (StatusCode::OK, Json(result)).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, Json(ConvertImageError::from(error))).into_response(),
        Err(error) => {
            tracing::error!("conversion worker failed: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ConvertImageError::from(
                chromium_icon::IconConversionError::from("转换任务失败，原输出已保留")
            ))).into_response()
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
        let safe_logo_name = match sanitize_convert_output_basename(&payload.logo_name) {
            Ok(name) => name,
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        };
        let logo_path_buf = logo_dir.join(&safe_logo_name);
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
        
        let format = safe_logo_name.rsplit('.').next().unwrap_or("png").to_ascii_lowercase();
        
        let mut fix_logo_path = std::path::PathBuf::from(logo_path_str);
        if format == "svg" {
            fix_logo_path.set_file_name("tmp.png");
            let logo_owned = logo_path_str.to_string();
            let png_name = fix_logo_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("tmp.png")
                .to_string();
            let svg_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                svg_png::convert_svg_to_png(&logo_owned, &png_name);
                chromium_icon::convert_svg_to_chromium_icon(&logo_owned, "product.icon");
            }));
            if svg_res.is_err() {
                return (StatusCode::BAD_REQUEST, "Failed to parse or convert SVG logo").into_response();
            }
        }
        
        fix_logo_path.to_str().map(|s| s.to_string())
    } else {
        None
    };
    
    let document_path = if !payload.document_name.is_empty() && !payload.document_data.is_empty() {
        let safe_doc_name = match sanitize_convert_output_basename(&payload.document_name) {
            Ok(name) => name,
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        };
        let document_path_buf = logo_dir.join(&safe_doc_name);
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
        let safe_logo_name = sanitize_convert_output_basename(&payload.logo_name).unwrap_or_else(|_| "oem".to_string());
        let oem_name = safe_logo_name.split('.').next().unwrap_or("oem");
        let generator = ThemeGenerator::new(&theme_dir, oem_name);
        
        match generator.generate_all(logo, document_path.as_deref()).await {
            Ok(_) => (StatusCode::OK, "OEM theme resources created successfully in oem/theme/".to_string()).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to generate theme resources: {}", e)).into_response(),
        }
    } else {
        (StatusCode::BAD_REQUEST, "Logo is required").into_response()
    }
}

pub async fn add_rounded_corners(Json(payload): Json<CornerRequest>) -> impl IntoResponse {
    let safe_logo_name = match sanitize_convert_output_basename(&payload.logo_name) {
        Ok(name) => name,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    
    let work_dir = match convert_work_dir() {
        Ok(dir) => dir,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to get convert dir: {}", e)).into_response(),
    };
    if let Err(e) = std::fs::create_dir_all(&work_dir) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to create convert dir: {}", e)).into_response();
    }
    let logo_path_buf = work_dir.join(&safe_logo_name);
    
    let logo_path = match logo_path_buf.to_str() {
        Some(path) => path.to_string(),
        None => return (StatusCode::BAD_REQUEST, "Invalid logo path").into_response(),
    };
    
    let logo_data = match STANDARD.decode(&payload.logo_data) {
        Ok(data) => data,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("Invalid base64 data: {}", e)).into_response(),
    };
    
    if let Err(e) = std::fs::write(&logo_path, &logo_data) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write file: {}", e)).into_response();
    }
    
    let radius = payload.radius.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        image_util::apply_rounded_corners(&logo_path, &radius)
    }));
    match result {
        Ok(outpath) => (StatusCode::OK, outpath).into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "Failed to apply rounded corners: invalid image or radius").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversion_request(source: &str, output: &str, format: &str) -> ConvertRequest {
        ConvertRequest {
            logo_name: source.into(),
            logo_data: STANDARD.encode("not an image"),
            output_path: output.into(),
            format: format.into(),
            emit_path_colors: None,
            allow_approximation: false,
        }
    }

    #[test]
    fn rejects_invalid_conversion_contract_before_creating_files() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let output_dir = temporary.0.join("not-created");
        for (source, output, format) in [
            ("image.svg", "result.png", "unknown"),
            ("image.svg", "result.svg", "PNG"),
            ("image.png", "image-ori.png", "PNG"),
            ("image.png", "result.icon", "ICON"),
            ("image.svg", "result.svg", "SVG"),
            ("image.icon", "result.ico", "ICO"),
            ("image.svg", "../result.png", "PNG"),
            ("image.svg", "file:result.png", "PNG"),
        ] {
            assert!(run_image_conversion_in_directory(
                conversion_request(source, output, format), &output_dir
            ).is_err(), "{source} -> {output} ({format}) must be rejected");
            assert!(!output_dir.exists());
        }
        assert!(validate_conversion_request(&conversion_request("image.SVG", "result.ICON", "icon")).is_ok());
    }

    #[test]
    fn rejects_nonportable_and_device_filenames() {
        for name in ["../a.icon", "..\\a.icon", "a.icon:stream", "NUL.png", "com1.svg", "LPT².ico", "a.icon.", "a?.svg", "a\n.svg"] {
            assert!(sanitize_convert_output_basename(name).is_err(), "{name:?} must be rejected");
        }
        assert!(sanitize_convert_output_basename("外观颜色-亮色模式.icon").is_ok());
    }

    #[test]
    fn malformed_input_preserves_existing_source_and_output() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let original = temporary.0.join("image-ori.png");
        let output = temporary.0.join("result.ico");
        std::fs::write(&original, b"previous original").unwrap();
        std::fs::write(&output, b"previous output").unwrap();
        assert!(run_image_conversion_in_directory(
            conversion_request("image.png", "result.ico", "ICO"), &temporary.0
        ).is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"previous original");
        assert_eq!(std::fs::read(output).unwrap(), b"previous output");
        assert_eq!(std::fs::read_dir(&temporary.0).unwrap().count(), 2);
    }

    const GRADIENT_SOURCE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><defs><linearGradient id="paint"><stop stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient></defs><rect width="16" height="16" fill="url(#paint)"/></svg>"##;

    #[test]
    fn upload_retains_error_classification_and_existing_artifacts() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let original = temporary.0.join("image-ori.svg");
        let output = temporary.0.join("result.icon");
        std::fs::write(&original, b"previous original").unwrap();
        std::fs::write(&output, b"previous output").unwrap();
        for (source, code) in [(GRADIENT_SOURCE, "requires_approximation"), ("not SVG", "conversion_failed")] {
            let mut request = conversion_request("image.svg", "result.icon", "ICON");
            request.logo_data = STANDARD.encode(source);
            let error = run_image_conversion_in_directory(request, &temporary.0).err().unwrap();
            let json = serde_json::to_value(ConvertImageError::from(error)).unwrap();
            assert_eq!(json["code"], code);
            assert!(json["message"].as_str().is_some_and(|message| !message.is_empty()));
            assert_eq!(std::fs::read(&original).unwrap(), b"previous original");
            assert_eq!(std::fs::read(&output).unwrap(), b"previous output");
        }
    }

    fn batch_request(root: &std::path::Path, files: Option<Vec<String>>, approximate: bool) -> IconPreviewBatchConvertRequest {
        IconPreviewBatchConvertRequest {
            directory: root.to_string_lossy().into_owned(),
            emit_path_colors: Some(true),
            allow_approximation: approximate,
            files,
        }
    }

    async fn batch_json(request: IconPreviewBatchConvertRequest) -> serde_json::Value {
        let response = convert_icon_directory(request);
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn targeted_retry_only_rewrites_selected_svg_outputs() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let root = &temporary.0;
        std::fs::write(root.join("gradient.svg"), GRADIENT_SOURCE).unwrap();
        std::fs::write(root.join("good.svg"), r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16"/></svg>"#).unwrap();
        std::fs::write(root.join("bad.svg"), "not SVG").unwrap();
        std::fs::write(root.join("gradient.icon"), b"old gradient").unwrap();
        std::fs::write(root.join("bad.icon"), b"old invalid output").unwrap();
        let strict = batch_json(batch_request(root, None, false)).await;
        assert_eq!(strict["svg_found"], 3);
        assert_eq!(strict["converted"], serde_json::json!(["good.icon"]));
        let errors = strict["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|error| error["file"] == "gradient.svg" && error["code"] == "requires_approximation"));
        assert!(errors.iter().any(|error| error["file"] == "bad.svg" && error["code"] == "conversion_failed"));
        assert_eq!(std::fs::read(root.join("gradient.icon")).unwrap(), b"old gradient");
        // A previously successful result must not be regenerated during retry.
        std::fs::write(root.join("good.icon"), b"keep successful output").unwrap();
        let retry = batch_json(batch_request(root, Some(vec!["gradient.svg".into(), "gradient.svg".into()]), true)).await;
        assert_eq!(retry["svg_found"], 1);
        assert_eq!(retry["converted"], serde_json::json!(["gradient.icon"]));
        assert_eq!(retry["errors"], serde_json::json!([]));
        assert!(std::fs::read_to_string(root.join("gradient.icon")).unwrap().contains("Approximation requested"));
        assert_eq!(std::fs::read(root.join("good.icon")).unwrap(), b"keep successful output");
        assert_eq!(std::fs::read(root.join("bad.icon")).unwrap(), b"old invalid output");
        assert_eq!(std::fs::read_to_string(root.join("gradient.svg")).unwrap(), GRADIENT_SOURCE);
        registered_preview_directories().write().unwrap().remove(&canonical_directory(root).unwrap());
    }

    #[test]
    fn invalid_retry_selection_is_rejected_before_any_conversion() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let root = &temporary.0;
        std::fs::write(root.join("chosen.svg"), GRADIENT_SOURCE).unwrap();
        std::fs::write(root.join("chosen.icon"), b"unchanged").unwrap();
        for selected in [
            vec![],
            vec!["chosen.svg".into(), "../outside.svg".into()],
            vec!["chosen.svg".into(), "chosen.icon".into()],
            vec!["chosen.svg".into(), "missing.svg".into()],
        ] {
            let response = convert_icon_directory(batch_request(root, Some(selected), true));
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(std::fs::read(root.join("chosen.icon")).unwrap(), b"unchanged");
        }
        registered_preview_directories().write().unwrap().remove(&canonical_directory(root).unwrap());
    }

    #[test]
    fn failed_output_publication_restores_previous_source() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let staging = ConversionStaging::new(&temporary.0).unwrap();
        let original = temporary.0.join("image-ori.svg");
        let output = temporary.0.join("result.icon");
        let staged_input = staging.0.join("image-ori.svg");
        std::fs::write(&original, b"previous original").unwrap();
        std::fs::write(&output, b"previous output").unwrap();
        std::fs::write(&staged_input, b"new original").unwrap();
        assert!(publish_conversion(&staging, &staged_input, &original,
            &staging.0.join("missing.icon"), &output).is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"previous original");
        assert_eq!(std::fs::read(output).unwrap(), b"previous output");
    }

    #[test]
    fn preview_requires_registered_directory_and_image_file() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let root = canonical_directory(&temporary.0).unwrap();
        let root_string = root.to_string_lossy();
        assert!(resolve_icon_preview_directory(Some(&root_string)).is_err());
        register_preview_directory(&root).unwrap();
        assert_eq!(resolve_icon_preview_directory(Some(&root_string)).unwrap(), root);
        std::fs::write(root.join("private.txt"), b"private").unwrap();
        std::fs::write(root.join("asset.icon"), b"CANVAS_DIMENSIONS, 16").unwrap();
        assert!(resolve_preview_asset(&root, "private.txt").is_err());
        assert!(resolve_preview_asset(&root, "../asset.icon").is_err());
        assert_eq!(resolve_preview_asset(&root, "asset.icon").unwrap(), root.join("asset.icon"));
        registered_preview_directories().write().unwrap().remove(&root);
    }

    #[cfg(unix)]
    #[test]
    fn preview_and_outputs_reject_symlinks_outside_directory() {
        let temporary = ConversionStaging::new(&std::env::temp_dir()).unwrap();
        let root_dir = temporary.0.join("authorized");
        std::fs::create_dir(&root_dir).unwrap();
        let root = canonical_directory(&root_dir).unwrap();
        let outside = temporary.0.join("outside.svg");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("asset.svg")).unwrap();
        assert!(resolve_preview_asset(&root, "asset.svg").is_err());
        assert!(checked_output_path(&root, "asset.svg").is_err());
    }

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
