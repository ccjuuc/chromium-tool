use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ConvertRequest {
    pub logo_name: String,
    pub logo_data: String,
    pub output_path: String,
    #[serde(default = "default_format")]
    pub format: String,
    /// 仅当 `format` 为 **ICON**（SVG→`.icon`）时生效。默认 `true`。为 `false`
    /// 时不写入 `PATH_COLOR_ARGB`，由 Chromium 运行时模板色上色。
    #[serde(default)]
    pub emit_path_colors: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct OemRequest {
    pub logo_name: String,
    pub logo_data: String,
    pub document_name: String,
    pub document_data: String,
}

#[derive(Debug, Deserialize)]
pub struct CornerRequest {
    pub logo_name: String,
    pub logo_data: String,
    pub radius: String,
}

fn default_format() -> String {
    "png".to_string()
}

