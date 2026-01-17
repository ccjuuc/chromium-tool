use std::path::Path;
use std::process::Command;
use anyhow::{Context, Result};
use crate::config::AppConfig;
use appdmg_rs::{DmgConfig, DmgContent, DmgWindow, DmgWindowSize};


#[cfg(target_os = "windows")]
mod os {
    pub const SHELL: [&str; 2] = ["cmd.exe", "/c"];
    pub const INSTALLER_PROJECT: &str = "installer_with_sign";
}

#[cfg(target_os = "macos")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const INSTALLER_PROJECT: &str = "chrome/installer/mac";
}

#[cfg(target_os = "linux")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const INSTALLER_PROJECT: &str = "chrome/installer/linux:stable";
}

#[derive(Clone)]
pub struct InstallerBuilder {
    #[allow(dead_code)]
    pub(crate) config: AppConfig,
}

impl InstallerBuilder {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }
    
    /// 执行 ninja 命令（支持命令列表）
    async fn run_ninja(
        &self,
        src_path: &Path,
        out_dir: &str,
        targets: &[&str],
        step_name: &str,
    ) -> Result<()> {
        for (index, target) in targets.iter().enumerate() {
            let command = format!("ninja -C {} {}", out_dir, target);
            let step_label = if targets.len() > 1 {
                format!("{} ({}/{})", step_name, index + 1, targets.len())
            } else {
                step_name.to_string()
            };
            
            tracing::info!("═══════════════════════════════════════════════════════");
            tracing::info!("📋 执行命令: {}", command);
            tracing::info!("📁 工作目录: {}", src_path.display());
            tracing::info!("🏷️  步骤: {}", step_label);
            tracing::info!("═══════════════════════════════════════════════════════");
            
            let start_time = std::time::Instant::now();
            let output = Command::new(os::SHELL[0])
                .arg(os::SHELL[1])
                .arg(&command)
                .current_dir(src_path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context(format!("Failed to spawn ninja for target: {}", target))?
                .wait_with_output()
                .context(format!("Failed to wait for ninja: {}", target))?;
            
            let duration = start_time.elapsed();
            let exit_code = output.status.code().unwrap_or(-1);
            
            // 打印命令输出
            if !output.stdout.is_empty() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                tracing::info!("✅ 标准输出:\n{}", stdout);
            }
            
            if !output.stderr.is_empty() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.success() {
                    tracing::warn!("⚠️  标准错误（警告）:\n{}", stderr);
                } else {
                    tracing::error!("❌ 标准错误:\n{}", stderr);
                }
            }
            
            tracing::info!("⏱️  执行时间: {:.2} 秒", duration.as_secs_f64());
            tracing::info!("🔢 退出码: {}", exit_code);
            
            if !output.status.success() {
                tracing::error!("❌ {} 执行失败", step_label);
                return Err(anyhow::anyhow!(
                    "{} failed with exit code {}: {}",
                    step_label,
                    exit_code,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            
            tracing::debug!("{} 执行成功", step_label);
            if index < targets.len() - 1 {
                tracing::info!("⏭️  继续执行下一个目标...\n");
            } else {
                tracing::info!("═══════════════════════════════════════════════════════\n");
            }
        }
        
        Ok(())
    }
    
    pub async fn build_installer(&self, src_path: &Path, out_dir: &str, installer_format: Option<&str>) -> Result<()> {
        // 先执行 ninja 构建 installer/mac（生成打包工具和目录）
        self.run_ninja(
            src_path,
            out_dir,
            &[os::INSTALLER_PROJECT],
            "installer build",
        ).await?;
        
        // macOS 需要额外生成 DMG 或 PKG
        #[cfg(target_os = "macos")]
        {
            let format = installer_format.unwrap_or("dmg"); // 默认为 dmg
            match format {
                "dmg" => {
                    self.create_dmg(src_path, out_dir).await?;
                }
                "pkg" => {
                    self.create_pkg(src_path, out_dir).await?;
                }
                _ => {
                    return Err(anyhow::anyhow!("不支持的安装包格式: {}，仅支持 dmg 或 pkg", format));
                }
            }
        }
        
        Ok(())
    }
    
    
    // Helper to generate background
    #[cfg(target_os = "macos")]
    fn create_dmg_background(&self, out_path: &Path) -> Result<()> {
        use image::{Rgba, RgbaImage};
        let width = 660u32;
        let height = 400u32;
        let mut img = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));
        
        let arrow_paths = vec![
            std::path::PathBuf::from("resources/dmg_arrow.png"), // Relative
            std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("resources/dmg_arrow.png"))).unwrap_or_default(),
             std::path::PathBuf::from("/Users/ext.shangzhijie1/chromium_tool/resources/dmg_arrow.png"),
        ];
        
        let (arrow_x, arrow_y) = (330u32, 190u32);
        
        for path in arrow_paths {
            if path.exists() {
                if let Ok(arrow_img) = image::open(&path) {
                    let arrow_rgba = arrow_img.to_rgba8();
                    let target = 64u32;
                    let arrow_rgba = if arrow_rgba.width() > target {
                        image::imageops::resize(&arrow_rgba, target, target, image::imageops::FilterType::Lanczos3)
                    } else { arrow_rgba };
                    
                    let (px0, py0) = (arrow_x.saturating_sub(arrow_rgba.width()/2), arrow_y.saturating_sub(arrow_rgba.height()/2));
                    for y in 0..arrow_rgba.height() {
                        for x in 0..arrow_rgba.width() {
                            let (px, py) = (px0 + x, py0 + y);
                            if px < width && py < height {
                                let p = arrow_rgba.get_pixel(x, y);
                                let a = p[3] as f32 / 255.0;
                                if a > 0.0 {
                                    let bg = img.get_pixel(px, py);
                                    img.put_pixel(px, py, Rgba([
                                        (p[0] as f32 * a + bg[0] as f32 * (1.0-a)) as u8,
                                        (p[1] as f32 * a + bg[1] as f32 * (1.0-a)) as u8,
                                        (p[2] as f32 * a + bg[2] as f32 * (1.0-a)) as u8, 255]));
                                }
                            }
                        }
                    }
                    tracing::info!("   ✅ 使用内置箭头资源");
                    break;
                }
            }
        }
        img.save(out_path).context("Failed to save background image")?;
        Ok(())
    }




    /// 创建 macOS DMG 安装包
    #[cfg(target_os = "macos")]
    async fn create_dmg(&self, src_path: &Path, out_dir: &str) -> Result<()> {

        use tokio::fs;
        
        tracing::info!("📦 开始创建 DMG 安装包...");
        
        let app_name = self.find_app_name(src_path, out_dir).await?;
        let app_path = src_path.join(out_dir).join(&app_name);
        
        if !app_path.exists() { return Err(anyhow::anyhow!("App not found: {}", app_path.display())); }
        
        let output_dir = src_path.join(out_dir).join("signed");
        fs::create_dir_all(&output_dir).await?;
        
        let dmg_name = self.generate_dmg_name(src_path, out_dir, &app_name).await?;
        let final_dmg_path = output_dir.join(&dmg_name);
        if final_dmg_path.exists() { fs::remove_file(&final_dmg_path).await?; }

        // --- 准备配置 ---
        // 1. 创建临时目录存放背景图
        let temp_dir = std::env::temp_dir().join(format!("joyme_config_{}", std::process::id()));
        if temp_dir.exists() { fs::remove_dir_all(&temp_dir).await?; }
        fs::create_dir_all(&temp_dir).await?;
        
        let background_path = temp_dir.join("background.png");
        self.create_dmg_background(&background_path)?;
        
        // 2. 查找图标
        let res_dir = app_path.join("Contents/Resources");
        let icon_path = ["AppIcon.icns", "app.icns", "icon.icns"].iter()
            .map(|n| res_dir.join(n)).find(|p| p.exists())
            .ok_or_else(|| anyhow::anyhow!("Icon not found"))?;

        // 3. 构建 Config 对象
        let volume_name = app_name.trim_end_matches(".app").to_string();
        
        let mut contents = vec![
            DmgContent {
                x: 170, y: 190,
                type_: "file".to_string(),
                path: app_path.to_string_lossy().to_string(),
                name: Some(app_name.clone()),
            },
            DmgContent {
                x: 490, y: 190,
                type_: "link".to_string(),
                path: "/Applications".to_string(),
                name: Some("Applications".to_string()),
            }
        ];
        
        // 4. 尝试从 src 目录读取 LICENSE 文件
        let src_dir = Path::new(&self.config.src.macos);
        let license_candidates = vec!["LICENSE.txt", "license.txt","LICENSE"];
        let mut license_file: Option<std::path::PathBuf> = None;
        
        for license_name in &license_candidates {
            let license_path = src_dir.join(license_name);
            if license_path.exists() {
                license_file = Some(license_path);
                break;
            }
        }
        
        if let Some(src_license) = license_file {
            // LICENSE 文件存在，将其复制到临时目录并添加到 DMG
            let license_dir = temp_dir.join("license");
            fs::create_dir_all(&license_dir).await?;
            let dest_license = license_dir.join(src_license.file_name().unwrap_or_default());
            fs::copy(&src_license, &dest_license).await?;
            
            tracing::info!("   ✅ 找到 LICENSE 文件: {}", src_license.display());
            
            contents.push(DmgContent {
                x: 330, y: 310,
                type_: "file".to_string(),
                path: license_dir.to_string_lossy().to_string(),
                name: Some("license".to_string()),
            });
        } else {
            // LICENSE 文件不存在，只输出 warn
            tracing::warn!("   ⚠️  在 src 目录 ({}) 中未找到 LICENSE 文件，跳过添加到 DMG", src_dir.display());
        }

        let config = DmgConfig {
            title: volume_name,
            icon: icon_path.to_string_lossy().to_string(),
            background: background_path.to_string_lossy().to_string(),
            icon_size: 128.0,
            window: DmgWindow { size: DmgWindowSize { width: 660, height: 400 } },
            contents,
        };
        
        // 4. 调用 appdmg-rs
        // 注意：appdmg-rs 内部可能会输出 "chflags: .fseventsd: No such file or directory" 的警告
        // 这是无害的：.fseventsd 是 macOS 的 Spotlight 索引目录，在 DMG 挂载时可能被自动创建，
        // 但库尝试设置其属性时目录可能已不存在，这是正常情况，不影响 DMG 创建
        appdmg_rs::build(&config, &final_dmg_path).await?;
        
        // 清理临时文件
        let _ = fs::remove_dir_all(&temp_dir).await;
        
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn create_dmg(&self, _src_path: &Path, _out_dir: &str) -> Result<()> {
        Ok(())
    }
    
    /// 创建 macOS PKG 安装包（仅 macOS）
    #[cfg(target_os = "macos")]
    async fn create_pkg(&self, src_path: &Path, out_dir: &str) -> Result<()> {
        use std::process::Command;
        use tokio::fs;
        
        tracing::info!("📦 开始创建 PKG 安装包...");
        
        // 查找 .app 文件
        let app_name = self.find_app_name(src_path, out_dir).await?;
        let app_path = src_path.join(out_dir).join(&app_name);
        
        if !app_path.exists() {
            return Err(anyhow::anyhow!("找不到应用文件: {}", app_path.display()));
        }
        
        tracing::info!("找到应用: {}", app_path.display());
        
        // 创建输出目录
        let output_dir = src_path.join(out_dir).join("signed");
        fs::create_dir_all(&output_dir).await
            .context("Failed to create signed output directory")?;
        
        // 生成 PKG 文件名
        let pkg_name = self.generate_pkg_name(src_path, out_dir, &app_name).await?;
        let pkg_path = output_dir.join(&pkg_name);
        
        // 使用 pkgbuild 创建 PKG
        tracing::info!("使用 pkgbuild 创建 PKG...");
        let base_name = app_name.trim_end_matches(".app");
        
        // 获取版本号
        let version = self.read_version_from_info_plist(src_path, out_dir, &app_name).await
            .unwrap_or_else(|_| "1.0.0".to_string());
        
        // 创建临时目录，将 .app 复制进去，使用 --root 方式打包
        let temp_dir = std::env::temp_dir().join(format!("joyme_pkg_stage_{}", std::process::id()));
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).await.ok();
        }
        fs::create_dir_all(&temp_dir).await
            .context("Failed to create temp directory for PKG")?;
        
        // 使用 ditto 复制 .app 到临时目录（保留符号链接，不展开）
        tracing::info!("📦 使用 ditto 复制应用到临时目录: {}", temp_dir.display());
        let temp_app_path = temp_dir.join(&app_name);
        let ditto_output = Command::new("ditto")
            .arg(&app_path)
            .arg(&temp_app_path)
            .output()
            .context("Failed to execute ditto")?;
        
        if !ditto_output.status.success() {
            let stderr = String::from_utf8_lossy(&ditto_output.stderr);
            return Err(anyhow::anyhow!("ditto failed: {}", stderr));
        }
        
        // 创建 component plist 文件，禁用 relocate（强制安装到 /Applications）
        let component_plist_path = output_dir.join("component.plist");
        let bundle_id = self.read_bundle_id_from_info_plist(src_path, out_dir, &app_name).await
            .unwrap_or_else(|_| format!("com.chromium.{}", base_name.to_lowercase().replace(" ", "")));
        
        let component_plist_content = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
    <dict>
        <key>BundleHasStrictIdentifier</key>
        <true/>
        <key>BundleIsRelocatable</key>
        <false/>
        <key>BundleIsVersionChecked</key>
        <false/>
        <key>BundleOverwriteAction</key>
        <string>upgrade</string>
        <key>RootRelativeBundlePath</key>
        <string>{}</string>
    </dict>
</array>
</plist>"#, app_name);
        
        fs::write(&component_plist_path, component_plist_content).await
            .context("Failed to write component plist")?;
        
        tracing::info!("📝 创建 component.plist，禁用 relocate");
        
        // 使用 pkgbuild 创建 PKG（--root + --component-plist）
        let output = Command::new("pkgbuild")
            .arg("--root")
            .arg(&temp_dir)
            .arg("--component-plist")
            .arg(&component_plist_path)
            .arg("--install-location")
            .arg("/Applications")
            .arg("--identifier")
            .arg(&bundle_id)
            .arg("--version")
            .arg(&version)
            .arg("--ownership")
            .arg("recommended")
            .arg(&pkg_path)
            .output()
            .context("Failed to execute pkgbuild")?;
        
        // 清理临时文件
        let _ = fs::remove_file(&component_plist_path).await;
        let _ = fs::remove_dir_all(&temp_dir).await;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(anyhow::anyhow!(
                "pkgbuild failed: stderr={}, stdout={}",
                stderr,
                stdout
            ));
        }
        
        if pkg_path.exists() {
            tracing::info!("✅ PKG 创建成功: {}", pkg_path.display());
        } else {
            return Err(anyhow::anyhow!("PKG 文件未生成: {}", pkg_path.display()));
        }
        
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn create_pkg(&self, _src_path: &Path, _out_dir: &str) -> Result<()> {
        Ok(())
    }
    
    /// 生成 PKG 文件名
    #[cfg(target_os = "macos")]
    async fn generate_pkg_name(&self, src_path: &Path, out_dir: &str, app_name: &str) -> Result<String> {
        // 从 app_name 提取基础名称（去掉 .app）
        let base_name = app_name.trim_end_matches(".app");
        
        // 尝试从 Info.plist 读取版本号
        let version = if let Ok(version) = self.read_version_from_info_plist(src_path, out_dir, app_name).await {
            version
        } else {
            // 使用时间戳作为版本号
            use std::time::{SystemTime, UNIX_EPOCH};
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            format!("{}", timestamp)
        };
        
        let pkg_name = format!("{}-{}.pkg", base_name, version);
        Ok(pkg_name)
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn generate_pkg_name(&self, _src_path: &Path, _out_dir: &str, _app_name: &str) -> Result<String> {
        Err(anyhow::anyhow!("仅支持 macOS"))
    }
    
    /// 查找 .app 文件名（优先查找主应用，排除 Helper 应用）
    #[cfg(target_os = "macos")]
    async fn find_app_name(&self, src_path: &Path, out_dir: &str) -> Result<String> {
        use tokio::fs;
        
        let out_path = src_path.join(out_dir);
        let mut entries = fs::read_dir(&out_path).await?;
        
        // 优先查找主应用（不包含 Helper、Plugin、Renderer 等关键词）
        let mut main_app: Option<String> = None;
        let mut fallback_app: Option<String> = None;
        
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                if let Some(file_name) = path.file_name() {
                    let name = file_name.to_string_lossy();
                    if name.ends_with(".app") {
                        let name_str = name.to_string();
                        // 排除 Helper、Plugin、Renderer 等辅助应用
                        if !name_str.to_lowercase().contains("helper") 
                            && !name_str.to_lowercase().contains("plugin")
                            && !name_str.to_lowercase().contains("renderer")
                            && !name_str.to_lowercase().contains("gpu") {
                            // 这是主应用
                            if main_app.is_none() {
                                main_app = Some(name_str);
                            }
                        } else {
                            // 这是辅助应用，作为备选
                            if fallback_app.is_none() {
                                fallback_app = Some(name_str);
                            }
                        }
                    }
                }
            }
        }
        
        // 优先返回主应用，如果没有主应用则返回第一个找到的 .app
        if let Some(app) = main_app {
            Ok(app)
        } else if let Some(app) = fallback_app {
            tracing::warn!("⚠️  未找到主应用，使用辅助应用: {}", app);
            Ok(app)
        } else {
            Err(anyhow::anyhow!("在 {} 中找不到 .app 文件", out_path.display()))
        }
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn find_app_name(&self, _src_path: &Path, _out_dir: &str) -> Result<String> {
        Err(anyhow::anyhow!("仅支持 macOS"))
    }
    
    
    /// 生成 DMG 文件名
    #[cfg(target_os = "macos")]
    async fn generate_dmg_name(&self, src_path: &Path, out_dir: &str, app_name: &str) -> Result<String> {
        // 从 app_name 提取基础名称（去掉 .app）
        let base_name = app_name.trim_end_matches(".app");
        
        // 尝试从 Info.plist 读取版本号
        let version = if let Ok(version) = self.read_version_from_info_plist(src_path, out_dir, app_name).await {
            version
        } else {
            // 使用时间戳作为版本号
            use std::time::{SystemTime, UNIX_EPOCH};
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            format!("{}", timestamp)
        };
        
        let dmg_name = format!("{}-{}.dmg", base_name, version);
        Ok(dmg_name)
    }
    
    /// 从 Info.plist 读取版本号（使用 plutil 命令）
    #[cfg(target_os = "macos")]
    async fn read_version_from_info_plist(&self, src_path: &Path, out_dir: &str, app_name: &str) -> Result<String> {
        use std::process::Command;
        
        // 构建 Info.plist 路径
        let info_plist_path = src_path.join(out_dir).join(app_name).join("Contents/Info.plist");
        
        if !info_plist_path.exists() {
            return Err(anyhow::anyhow!("Info.plist 文件不存在: {}", info_plist_path.display()));
        }
        
        // 使用 plutil 命令读取 CFBundleShortVersionString
        let output = Command::new("plutil")
            .arg("-extract")
            .arg("CFBundleShortVersionString")
            .arg("raw")
            .arg("-o")
            .arg("-")
            .arg(&info_plist_path)
            .output()
            .context("Failed to execute plutil")?;
        
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !version.is_empty() {
                return Ok(version);
            }
        }
        
        // 如果 CFBundleShortVersionString 失败，尝试 CFBundleVersion
        let output = Command::new("plutil")
            .arg("-extract")
            .arg("CFBundleVersion")
            .arg("raw")
            .arg("-o")
            .arg("-")
            .arg(&info_plist_path)
            .output()
            .context("Failed to execute plutil")?;
        
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !version.is_empty() {
                return Ok(version);
            }
        }
        
        Err(anyhow::anyhow!("无法从 Info.plist 读取版本号"))
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn read_version_from_info_plist(&self, _src_path: &Path, _out_dir: &str, _app_name: &str) -> Result<String> {
        Err(anyhow::anyhow!("仅支持 macOS"))
    }
    
    /// 从 Info.plist 读取 Bundle ID（使用 plutil 命令）
    #[cfg(target_os = "macos")]
    async fn read_bundle_id_from_info_plist(&self, src_path: &Path, out_dir: &str, app_name: &str) -> Result<String> {
        use std::process::Command;
        
        // 构建 Info.plist 路径
        let info_plist_path = src_path.join(out_dir).join(app_name).join("Contents/Info.plist");
        
        if !info_plist_path.exists() {
            return Err(anyhow::anyhow!("Info.plist 文件不存在: {}", info_plist_path.display()));
        }
        
        // 使用 plutil 命令读取 CFBundleIdentifier
        let output = Command::new("plutil")
            .arg("-extract")
            .arg("CFBundleIdentifier")
            .arg("raw")
            .arg("-o")
            .arg("-")
            .arg(&info_plist_path)
            .output()
            .context("Failed to execute plutil")?;
        
        if output.status.success() {
            let bundle_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !bundle_id.is_empty() {
                return Ok(bundle_id);
            }
        }
        
        Err(anyhow::anyhow!("无法从 Info.plist 读取 Bundle ID"))
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn read_bundle_id_from_info_plist(&self, _src_path: &Path, _out_dir: &str, _app_name: &str) -> Result<String> {
        Err(anyhow::anyhow!("仅支持 macOS"))
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn generate_dmg_name(&self, _src_path: &Path, _out_dir: &str, _app_name: &str) -> Result<String> {
        Err(anyhow::anyhow!("仅支持 macOS"))
    }
    
    // 辅助函数：迭代复制目录（避免递归）
    async fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
        use tokio::fs;
        use std::collections::VecDeque;
        
        // 使用栈来模拟递归，避免递归调用
        let mut stack = VecDeque::new();
        stack.push_back((src.to_path_buf(), dst.to_path_buf()));
        
        while let Some((src_path, dst_path)) = stack.pop_back() {
            // 确保目标目录存在
            if !dst_path.exists() {
                fs::create_dir_all(&dst_path).await
                    .context(format!("Failed to create directory: {}", dst_path.display()))?;
            }
            
            // 读取源目录的所有条目
            let mut entries = fs::read_dir(&src_path).await
                .context(format!("Failed to read directory: {}", src_path.display()))?;
            
            while let Some(entry) = entries.next_entry().await? {
                let entry_path = entry.path();
                let entry_dst = dst_path.join(
                    entry_path.file_name().ok_or_else(|| {
                        anyhow::anyhow!("Invalid file name in path: {:?}", entry_path)
                    })?
                );
                
                if entry_path.is_dir() {
                    // 将子目录添加到栈中处理
                    stack.push_back((entry_path, entry_dst));
                } else {
                    // 复制文件
                    fs::copy(&entry_path, &entry_dst).await
                        .context(format!("Failed to copy file from {} to {}", 
                            entry_path.display(), entry_dst.display()))?;
                }
            }
        }
        
        Ok(())
    }
    
    /// 执行多个安装包构建目标（按顺序执行）
    #[allow(dead_code)] // 保留用于将来支持多个安装包目标的场景
    pub async fn build_installers(
        &self,
        src_path: &Path,
        out_dir: &str,
        targets: &[&str],
    ) -> Result<()> {
        self.run_ninja(src_path, out_dir, targets, "installer build").await
    }
    
    /// 组合多个架构的 app 并生成 universal pkg（仅 macOS）
    #[cfg(target_os = "macos")]
    pub async fn combine_universal_pkg(
        &self,
        src_path: &Path,
        architectures: &[String],
    ) -> Result<()> {
        use std::process::Command;
        use tokio::fs;
        
        tracing::info!("🔗 开始组合 universal pkg，架构: {:?}", architectures);
        
        if architectures.len() < 2 {
            return Err(anyhow::anyhow!("需要至少2个架构才能组合"));
        }
        
        let universal_out_dir = "out/Release_universal";
        let universal_app_path = src_path.join(universal_out_dir).join("Chromium.app");
        
        // 创建 universal 输出目录
        fs::create_dir_all(&universal_app_path).await
            .context("Failed to create universal app directory")?;
        
        // 1. 合并主可执行文件
        let mut lipo_args = vec!["-create".to_string()];
        for arch in architectures {
            let arch_out_dir = match arch.as_str() {
                "arm64" => "out/Release_arm64",
                "x64" => "out/Release_x64",
                _ => continue,
            };
            let executable_path = src_path.join(arch_out_dir)
                .join("Chromium.app/Contents/MacOS/Chromium");
            if executable_path.exists() {
                lipo_args.push(executable_path.to_string_lossy().to_string());
            }
        }
        
        if lipo_args.len() < 3 {
            return Err(anyhow::anyhow!("无法找到足够的可执行文件进行合并"));
        }
        
        let output_executable = universal_app_path.join("Contents/MacOS/Chromium");
        fs::create_dir_all(output_executable.parent().unwrap()).await?;
        
        lipo_args.push("-output".to_string());
        lipo_args.push(output_executable.to_string_lossy().to_string());
        
        tracing::info!("📋 执行命令: lipo {}", lipo_args.join(" "));
        let output = Command::new("lipo")
            .args(&lipo_args)
            .output()
            .context("Failed to execute lipo")?;
        
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "lipo failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        
        // 2. 复制资源文件（从第一个架构复制）
        let first_arch_dir = match architectures[0].as_str() {
            "arm64" => "out/Release_arm64",
            "x64" => "out/Release_x64",
            _ => return Err(anyhow::anyhow!("不支持的架构")),
        };
        
        let source_app = src_path.join(first_arch_dir).join("Chromium.app");
        if source_app.exists() {
            // 复制 Info.plist
            if let Some(info_plist) = source_app.join("Contents/Info.plist").to_str() {
                if std::path::Path::new(info_plist).exists() {
                    let dest_info_plist = universal_app_path.join("Contents/Info.plist");
                    fs::copy(info_plist, &dest_info_plist).await?;
                }
            }
            
            // 复制 Resources 目录
            let source_resources = source_app.join("Contents/Resources");
            let dest_resources = universal_app_path.join("Contents/Resources");
            if source_resources.exists() {
                if dest_resources.exists() {
                    fs::remove_dir_all(&dest_resources).await?;
                }
                Self::copy_dir_all(&source_resources, &dest_resources).await?;
            }
            
            // 复制 Frameworks 目录（如果需要）
            let source_frameworks = source_app.join("Contents/Frameworks");
            let dest_frameworks = universal_app_path.join("Contents/Frameworks");
            if source_frameworks.exists() {
                if dest_frameworks.exists() {
                    fs::remove_dir_all(&dest_frameworks).await?;
                }
                Self::copy_dir_all(&source_frameworks, &dest_frameworks).await?;
            }
        }
        
        // 3. 生成 universal pkg
        tracing::info!("📦 生成 universal pkg...");
        self.run_ninja(
            src_path,
            universal_out_dir,
            &[os::INSTALLER_PROJECT],
            "universal pkg build",
        ).await?;
        
        tracing::info!("✅ Universal pkg 生成完成");
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    pub async fn combine_universal_pkg(
        &self,
        _src_path: &Path,
        _architectures: &[String],
    ) -> Result<()> {
        Err(anyhow::anyhow!("Universal pkg 组合仅支持 macOS"))
    }
}

