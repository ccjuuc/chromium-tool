use validator::Validate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Validate, Serialize, Deserialize, Clone)]
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

impl BuildRequest {
    /// 校验参数中是否包含危险的 Shell 元字符，防止命令注入
    pub fn validate_shell_safety(&self) -> Result<(), String> {
        let is_unsafe = |s: &str| {
            s.chars().any(|c| matches!(c, ';' | '|' | '&' | '`' | '$' | '\n' | '\r' | '<' | '>'))
        };

        if is_unsafe(&self.branch) || self.branch.starts_with('-') {
            return Err("Invalid branch name".to_string());
        }

        if let Some(commit) = &self.commit_id {
            if !commit.is_empty() && (is_unsafe(commit) || commit.starts_with('-')) {
                return Err("Invalid commit_id".to_string());
            }
        }

        for arch in &self.architectures {
            if !matches!(arch.as_str(), "x64" | "x86" | "arm64" | "arm") {
                return Err(format!("Unsupported architecture: {}", arch));
            }
        }

        if let Some(args) = &self.custom_args {
            for arg in args {
                if is_unsafe(arg) {
                    return Err(format!("Unsafe custom argument: {}", arg));
                }
            }
        }

        Ok(())
    }
}

