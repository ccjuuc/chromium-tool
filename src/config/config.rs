use config::{Config, File, Environment};
use serde::{Deserialize, Serialize};
use std::env;
use anyhow::{Context, Result};

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub sign: Option<String>,
    pub custom_args: Vec<String>,
    pub build_args: Vec<String>,
    pub oem: OemConfig,
    pub clean: CleanConfig,
    #[allow(dead_code)]
    pub git: GitConfig,
    pub src: PlatformPaths,
    pub dev_tools: PlatformPaths,
    pub python: Option<PlatformPaths>,
    pub backup_path: PlatformPaths,
    pub server: ServerConfig,
    pub email: EmailConfig,
    pub gn_default_args: PlatformArgs,
    #[serde(default)]
    pub build_steps: PlatformBuildSteps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OemConfig {
    pub oem_key: String,
    pub oems: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CleanConfig {
    pub path: Vec<String>,
    pub out_path: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitConfig {
    #[allow(dead_code)]
    pub addr: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformPaths {
    #[serde(default)]
    pub windows: String,
    #[serde(default)]
    pub linux: String,
    #[serde(default)]
    pub macos: String,
    #[serde(default)]
    pub db: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub windows: Vec<String>,
    pub macos: Vec<String>,
    pub linux: Vec<String>,
    pub db_server: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmailConfig {
    #[allow(dead_code)]
    pub web: String,
    pub smtp: String,
    pub from: String,
    pub password: String,
    pub to: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformArgs {
    #[serde(default)]
    pub windows: Vec<String>,
    #[serde(default)]
    pub linux: Vec<String>,
    #[serde(default)]
    pub macos: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PlatformBuildSteps {
    #[serde(default)]
    pub windows: ArchitectureBuildSteps,
    #[serde(default)]
    pub linux: ArchitectureBuildSteps,
    #[serde(default)]
    pub macos: ArchitectureBuildSteps,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ArchitectureBuildSteps {
    // Windows: x64, x86
    #[serde(default)]
    pub x64: Vec<BuildStep>,
    #[serde(default)]
    pub x86: Vec<BuildStep>,
    
    // macOS: arm64, x64
    #[serde(default)]
    pub arm64: Vec<BuildStep>,
    
    // Linux: x64, arm64, arm
    #[serde(default)]
    pub arm: Vec<BuildStep>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildStep {
    pub name: String,
    pub step_type: String,  // "git", "ninja", "clean", "gn_gen", "installer"
    pub target: Option<String>,  // ninja 目标或 git 操作
    pub state: Option<String>,  // TaskState 名称
    pub skip_if: Option<String>,  // 跳过条件，如 "target_os=macos", "is_increment=true"
    #[allow(dead_code)]
    pub description: Option<String>,  // 步骤描述
}

impl AppConfig {
    pub async fn load(path: &str) -> Result<Self> {
        let config = Config::builder()
            .add_source(File::with_name(path))
            .add_source(Environment::with_prefix("PKG_SRV"))
            .build()
            .context("Failed to load config")?;
        
        let app_config: AppConfig = config.try_deserialize()
            .context("Failed to deserialize config")?;
        
        // 初始化环境变量
        app_config.init_env();
        
        Ok(app_config)
    }
    
    fn init_env(&self) {
        env::set_var("XN_BUILD", "1");
        
        if let Some(sign_server) = &self.sign {
            if !sign_server.is_empty() {
                env::set_var("SNOW_SIGN_ADDRESS", sign_server);
            }
        }
        
        // 设置 PATH
        let separator = if cfg!(windows) { ";" } else { ":" };
        
        if let Some(dev_path) = self.get_dev_tools_path() {
            if !dev_path.is_empty() {
                let current_path = env::var("PATH").unwrap_or_default();
                let env_addition = format!("{}{}{}", dev_path, separator, current_path);
                env::set_var("PATH", env_addition);
            }
        }
        
        if let Some(python_path) = self.get_python_path() {
            if !python_path.is_empty() {
                let current_path = env::var("PATH").unwrap_or_default();
                let env_addition = format!("{}{}{}", python_path, separator, current_path);
                env::set_var("PATH", env_addition);
            }
        }
    }
    
    pub fn get_src_path(&self) -> Result<&str> {
        let os = std::env::consts::OS;
        match os {
            "windows" => Ok(&self.src.windows),
            "linux" => Ok(&self.src.linux),
            "macos" => Ok(&self.src.macos),
            _ => Err(anyhow::anyhow!("Unsupported OS: {}", os)),
        }
    }
    
    pub fn get_backup_path(&self) -> Result<&str> {
        let os = std::env::consts::OS;
        match os {
            "windows" => Ok(&self.backup_path.windows),
            "linux" => Ok(&self.backup_path.linux),
            "macos" => Ok(&self.backup_path.macos),
            _ => Err(anyhow::anyhow!("Unsupported OS: {}", os)),
        }
    }
    
    pub fn get_gn_default_args(&self) -> Result<&[String]> {
        let os = std::env::consts::OS;
        match os {
            "windows" => Ok(&self.gn_default_args.windows),
            "linux" => Ok(&self.gn_default_args.linux),
            "macos" => Ok(&self.gn_default_args.macos),
            _ => Err(anyhow::anyhow!("Unsupported OS: {}", os)),
        }
    }
    
    pub fn get_db_path(&self) -> &str {
        &self.src.db
    }
    
    fn get_dev_tools_path(&self) -> Option<&str> {
        let os = std::env::consts::OS;
        match os {
            "windows" => Some(&self.dev_tools.windows),
            "linux" => Some(&self.dev_tools.linux),
            "macos" => Some(&self.dev_tools.macos),
            _ => None,
        }
    }
    
    fn get_python_path(&self) -> Option<&str> {
        let _os = std::env::consts::OS;
        self.python.as_ref().and_then(|p| {
            match _os {
                "linux" => Some(p.linux.as_str()),
                "macos" => Some(p.macos.as_str()),
                _ => None,
            }
        })
    }
    
    pub fn get_build_steps(&self, architecture: Option<&str>) -> Vec<BuildStep> {
        let os = std::env::consts::OS;
        let arch = architecture.unwrap_or("x64");  // 默认使用 x64
        
        match os {
            "windows" => {
                match arch {
                    "x64" => self.build_steps.windows.x64.clone(),
                    "x86" => self.build_steps.windows.x86.clone(),
                    _ => self.build_steps.windows.x64.clone(), // 默认 x64
                }
            },
            "macos" => {
                match arch {
                    "arm64" => {
                        if !self.build_steps.macos.arm64.is_empty() {
                            self.build_steps.macos.arm64.clone()
                        } else {
                            self.build_steps.macos.x64.clone()
                        }
                    },
                    "x64" => self.build_steps.macos.x64.clone(),
                    _ => self.build_steps.macos.x64.clone(), // 默认 x64
                }
            },
            "linux" => {
                match arch {
                    "x64" => self.build_steps.linux.x64.clone(),
                    "arm64" => {
                        if !self.build_steps.linux.arm64.is_empty() {
                            self.build_steps.linux.arm64.clone()
                        } else {
                            self.build_steps.linux.x64.clone()
                        }
                    },
                    "arm" => {
                        if !self.build_steps.linux.arm.is_empty() {
                            self.build_steps.linux.arm.clone()
                        } else {
                            self.build_steps.linux.x64.clone()
                        }
                    },
                    _ => self.build_steps.linux.x64.clone(), // 默认 x64
                }
            },
            _ => vec![],
        }
    }
}

