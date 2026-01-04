use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct FileCategories {
    pub zh_cn_files: Vec<String>,
    pub en_us_files: Vec<String>,
    pub en_gb_files: Vec<String>,
    pub grd_files: Vec<String>,
    pub grdp_files: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GenerateIdRequest {
    pub message: String,
    pub meaning: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GenerateIdResponse {
    pub message_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchIdRequest {
    pub search_text: String,
    // src_path 已移除，现在从 config.toml 获取
}

#[derive(Debug, Serialize)]
pub struct SearchIdResponse {
    pub ids: Vec<String>,
    pub messages: Vec<String>,
    pub grd_matches: Vec<String>,
}

