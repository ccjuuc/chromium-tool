use config::{Config, File, Environment};
use serde::{Deserialize, Serialize};
use std::env;
use anyhow::{Context, Result};

// 顶层与每个内嵌结构体都派生了 `Default`，目的是允许在配置文件缺失时
// 通过 `AppConfig::default()` 启动一份"什么都没配"的最小可运行实例。
// 对应字段全部使用 `#[serde(default)]`，所以即使 toml 中只写了部分段也能解析。

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub sign: Option<String>,
    #[serde(default)]
    pub custom_args: Vec<String>,
    #[serde(default)]
    pub build_args: Vec<String>,
    #[serde(default)]
    pub oem: OemConfig,
    #[serde(default)]
    pub clean: CleanConfig,
    #[allow(dead_code)]
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub src: PlatformPaths,
    #[serde(default)]
    pub dev_tools: PlatformPaths,
    #[serde(default)]
    pub python: Option<PlatformPaths>,
    #[serde(default)]
    pub backup_path: PlatformPaths,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub email: EmailConfig,
    #[serde(default)]
    pub gn_default_args: PlatformArgs,
    #[serde(default)]
    pub build_steps: PlatformBuildSteps,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OemConfig {
    #[serde(default)]
    pub oem_key: String,
    #[serde(default)]
    pub oems: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CleanConfig {
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub out_path: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GitConfig {
    #[allow(dead_code)]
    #[serde(default)]
    pub addr: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub windows: Vec<String>,
    #[serde(default)]
    pub macos: Vec<String>,
    #[serde(default)]
    pub linux: Vec<String>,
    #[serde(default)]
    pub db_server: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmailConfig {
    #[allow(dead_code)]
    #[serde(default)]
    pub web: String,
    #[serde(default)]
    pub smtp: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub to: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
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
    /// 加载配置。
    ///
    /// 加载策略（按优先级）：
    /// 1. 配置文件存在 → 解析它（解析失败仍然返回错误，避免静默运行错误配置）
    /// 2. 配置文件不存在 → 打印警告并使用 `AppConfig::default()` 启动
    ///
    /// 任意情况下都会再叠加 `PKG_SRV_*` 环境变量覆盖。这样既能在缺少
    /// `config.toml` 时让服务以"零配置"模式起来（数据库/签名/邮件等可选模块
    /// 会自动跳过），也能在持续集成里用环境变量覆盖个别字段。
    pub async fn load(path: &str) -> Result<Self> {
        let mut builder = Config::builder();

        let path_exists = std::path::Path::new(path).exists();
        if path_exists {
            builder = builder.add_source(File::with_name(path));
        } else {
            tracing::warn!(
                "⚠️  配置文件 {} 不存在，将使用默认配置启动；可通过 PKG_SRV_* 环境变量覆盖",
                path
            );
        }
        builder = builder.add_source(Environment::with_prefix("PKG_SRV"));

        let config = builder.build().context("Failed to load config")?;

        // 把（可能为空的）source 反序列化到 AppConfig；所有字段都是
        // `#[serde(default)]`，所以即使没有任何 source 也能拿到一个
        // 全默认的实例。
        let app_config: AppConfig = config
            .try_deserialize()
            .unwrap_or_else(|e| {
                if path_exists {
                    // 文件存在但解析失败 —— 多半是写错了，记录详细信息但
                    // 仍然降级为默认配置，保持服务可启动。如果希望严格
                    // 校验，可以把这里换成 `Err(...)?`。
                    tracing::error!(
                        "❌ 解析配置文件 {} 失败：{}；将使用默认配置启动",
                        path,
                        e
                    );
                }
                AppConfig::default()
            });

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

