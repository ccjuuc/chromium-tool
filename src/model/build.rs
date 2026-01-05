use validator::Validate;
use serde::Deserialize;

#[derive(Debug, Validate, Deserialize, Clone)]
pub struct BuildRequest {
    #[validate(length(min = 1, max = 100))]
    pub branch: String,
    
    #[validate(length(max = 40))]
    pub commit_id: Option<String>,
    
    #[validate(length(max = 50))]
    pub pkg_flag: String,
    
    pub is_update: bool,
    pub is_x64: bool,
    
    pub architectures: Vec<String>,  // 架构列表: ["x64", "arm64"]，至少包含一个
    
    #[validate(length(min = 1, max = 20))]
    pub platform: String,
    
    pub is_increment: bool,
    pub is_signed: bool,
    
    #[validate(length(min = 1, max = 100))]
    pub server: String,
    
    pub custom_args: Option<Vec<String>>,
    
    pub emails: Option<Vec<String>>,  // 邮箱列表，支持多个
    
    #[serde(default)]
    pub installer_format: Option<String>,  // 安装包格式：dmg 或 pkg（仅 macOS）
}

