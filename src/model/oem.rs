use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ConvertRequest {
    pub logo_name: String,
    pub logo_data: String,
    pub output_path: String,
    #[serde(default = "default_format")]
    pub format: String,
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

