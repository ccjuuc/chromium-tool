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
    
    #[validate(length(max = 50))]
    pub oem_name: String,
    
    pub custom_args: Option<Vec<String>>,
    
    #[validate(email)]
    pub email: Option<String>,
}

