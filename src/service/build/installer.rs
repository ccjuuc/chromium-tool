use std::path::Path;
use std::process::Command;
use anyhow::{Context, Result};
use crate::config::AppConfig;

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
    
    /// 创建 macOS DMG 安装包（仅 macOS）
    #[cfg(target_os = "macos")]
    async fn create_dmg(&self, src_path: &Path, out_dir: &str) -> Result<()> {
        use std::process::Command;
        use tokio::fs;
        
        tracing::info!("📦 开始创建 DMG 安装包 (Native)...");
        
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
        
        // 从 app_name 提取版本信息（如果可能）
        let dmg_name = self.generate_dmg_name(src_path, out_dir, &app_name).await?;
        let final_dmg_path = output_dir.join(&dmg_name);
        
        // 使用临时文件进行构建（UDRW 格式，可读写，用于调整图标位置）
        let temp_dmg_name = format!("temp_{}", dmg_name);
        let temp_dmg_path = output_dir.join(&temp_dmg_name);
        
        // 清理旧文件
        if temp_dmg_path.exists() {
            fs::remove_file(&temp_dmg_path).await?;
        }
        if final_dmg_path.exists() {
            fs::remove_file(&final_dmg_path).await?;
        }
        
        // 创建临时目录用于 staging
        let temp_dir = std::env::temp_dir().join(format!("joyme_dmg_stage_{}", std::process::id()));
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).await?;
        }
        fs::create_dir_all(&temp_dir).await?;
        
        // 使用 ditto 复制应用到临时目录（保留符号链接，不展开）
        tracing::info!("使用 ditto 复制应用到临时目录: {}", temp_dir.display());
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
        
        // 创建 /Applications 软链接
        let symlink_path = temp_dir.join("Applications");
        tracing::info!("创建 Applications 软链接: {}", symlink_path.display());
        if let Err(e) = tokio::fs::symlink("/Applications", &symlink_path).await {
            tracing::warn!("⚠️  创建 Applications 软链接失败: {}", e);
        }
        
        // 使用 hdiutil 创建可读写 DMG (UDRW)
        // 这里的逻辑替代了 pkg-dmg，避免了 bless 在 Apple Silicon 上的错误
        tracing::info!("使用 hdiutil 创建临时可读写 DMG...");
        let volume_name = app_name.trim_end_matches(".app");
        
        let output = Command::new("hdiutil")
            .arg("create")
            .arg("-srcfolder")
            .arg(&temp_dir)
            .arg("-volname")
            .arg(volume_name)
            .arg("-format")
            .arg("UDRW")
            .arg("-ov") // Overwrite
            .arg(&temp_dmg_path)
            .output()
            .context("Failed to execute hdiutil create")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(anyhow::anyhow!(
                "hdiutil create failed: stderr={}, stdout={}",
                stderr,
                stdout
            ));
        }
        
        // 设置 DMG 图标位置（应用在左侧，Applications 在右侧）
        tracing::info!("🎨 设置 DMG 图标布局...");
        if let Err(e) = self.set_dmg_icon_positions(&temp_dmg_path, &app_name).await {
            tracing::warn!("⚠️  设置 DMG 图标位置失败: {}，但将继续生成...", e);
        }
        
        // 转换前确保临时 DMG 没有被挂载
        let volume_name = app_name.trim_end_matches(".app");
        let _ = Command::new("hdiutil")
            .arg("detach")
            .arg(format!("/Volumes/{}", volume_name))
            .arg("-force")
            .output();
        
        // 等待系统完全释放资源
        std::thread::sleep(std::time::Duration::from_secs(1));
        
        // 转换为最终的只读压缩 DMG (UDZO)
        tracing::info!("🔒 转换 DMG 为只读压缩格式 (UDZO)...");
        let convert_output = Command::new("hdiutil")
            .arg("convert")
            .arg(&temp_dmg_path)
            .arg("-format")
            .arg("UDZO")
            .arg("-ov") // 覆盖已存在的文件
            .arg("-o")
            .arg(&final_dmg_path)
            .output()
            .context("Failed to convert DMG to UDZO")?;
            
        if !convert_output.status.success() {
            let stderr = String::from_utf8_lossy(&convert_output.stderr);
            return Err(anyhow::anyhow!(
                "DMG conversion failed: {}",
                stderr
            ));
        }
        
        // 清理临时文件
        let _ = fs::remove_file(&temp_dmg_path).await;
        // 如果 hdiutil 自动添加了 .dmg 后缀，可能存在 temp_dmg_path.dmg，尝试清理
        let temp_dmg_path_extra = output_dir.join(format!("{}.dmg", temp_dmg_name));
        if temp_dmg_path_extra.exists() {
             let _ = fs::remove_file(&temp_dmg_path_extra).await;
        }
        
        let _ = fs::remove_dir_all(&temp_dir).await;
        
        if final_dmg_path.exists() {
            tracing::info!("✅ DMG 创建成功: {}", final_dmg_path.display());
            
            // 验证最终 DMG 中是否包含 .DS_Store 文件
            tracing::info!("🔍 验证最终 DMG 中的 .DS_Store 文件...");
            let verify_output = Command::new("hdiutil")
                .arg("attach")
                .arg("-nobrowse")
                .arg("-noverify")
                .arg("-noautoopen")
                .arg("-readonly")
                .arg(&final_dmg_path)
                .output();
            
            if let Ok(output) = verify_output {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    // 从输出中提取挂载点
                    if let Some(idx) = stdout.find("/Volumes/") {
                        let verify_mount = stdout[idx..].trim().split_whitespace().next().unwrap_or("");
                        let verify_ds_store = format!("{}/.DS_Store", verify_mount);
                        
                        if std::path::Path::new(&verify_ds_store).exists() {
                            if let Ok(metadata) = std::fs::metadata(&verify_ds_store) {
                                tracing::info!("   ✅ 最终 DMG 中包含 .DS_Store 文件");
                                tracing::info!("   大小: {} 字节", metadata.len());
                            }
                        } else {
                            tracing::warn!("   ⚠️  最终 DMG 中不包含 .DS_Store 文件！");
                        }
                        
                        // 卸载验证用的 DMG
                        let _ = Command::new("hdiutil")
                            .arg("detach")
                            .arg(verify_mount)
                            .arg("-force")
                            .output();
                    }
                }
            }
        } else {
            return Err(anyhow::anyhow!("DMG 文件未生成: {}", final_dmg_path.display()));
        }
        
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
    
    /// 设置 DMG 图标位置（应用在左侧，Applications 在右侧）
    #[cfg(target_os = "macos")]
    async fn set_dmg_icon_positions(&self, dmg_path: &Path, app_name: &str) -> Result<()> {
        use std::process::Command;
        
        // 清理可能残留的挂载点（避免 "JoyME 1" 这样的命名）
        let volume_name = app_name.trim_end_matches(".app");
        tracing::info!("🧹 清理可能残留的挂载点...");
        for i in 0..10 {
            let vol_path = if i == 0 {
                format!("/Volumes/{}", volume_name)
            } else {
                format!("/Volumes/{} {}", volume_name, i)
            };
            let _ = Command::new("hdiutil")
                .arg("detach")
                .arg(&vol_path)
                .arg("-force")
                .output();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        
        // 使用 hdiutil attach 挂载 DMG
        let attach_output = Command::new("hdiutil")
            .arg("attach")
            .arg("-nobrowse")
            .arg("-noverify")
            .arg("-noautoopen")
            .arg(dmg_path)
            .output()
            .context("Failed to execute hdiutil attach")?;
        
        if !attach_output.status.success() {
            return Err(anyhow::anyhow!("Failed to attach DMG: {}", String::from_utf8_lossy(&attach_output.stderr)));
        }
        
        // 从输出中提取挂载点（查找 /Volumes/ 开头的路径）
        let stdout = String::from_utf8_lossy(&attach_output.stdout);
        tracing::debug!("hdiutil attach 输出: {}", stdout);
        
        let mount_point = stdout
            .lines()
            .find_map(|line| {
                // 查找包含 /Volumes/ 的行，提取挂载点路径
                if let Some(idx) = line.find("/Volumes/") {
                    // 从 /Volumes/ 开始到行尾就是挂载点
                    let path = line[idx..].trim();
                    if !path.is_empty() {
                        return Some(path.to_string());
                    }
                }
                None
            })
            .ok_or_else(|| anyhow::anyhow!("Failed to find mount point in: {}", stdout))?;
        
        tracing::info!("📂 DMG 挂载点: {}", mount_point);
        
        // 使用 AppleScript 设置图标位置（标准 DMG 布局）
        // 窗口大小: 660 x 400
        // 图标大小: 100
        // 应用图标和 Applications 图标居中排列
        // 1. 删除 .DS_Store，确保从干净状态开始
        let ds_store_path = format!("{}/.DS_Store", mount_point);
        let _ = Command::new("rm")
            .arg("-f")
            .arg(&ds_store_path)
            .output();
            
        // 2. 使用 AppleScript 设置图标位置
        // 窗口大小: 660 x 400
        // 图标大小: 100
        // 应用图标位置：左侧 (170, 190) - 居中显示
        // Applications 图标位置：右侧 (490, 190) - 拖放目标
        let applescript = format!(
            r#"
            tell application "Finder"
                set dmgPath to POSIX file "{}" as alias
                open dmgPath
                delay 0.5
                
                set targetWindow to container window of dmgPath
                set current view of targetWindow to icon view
                set toolbar visible of targetWindow to false
                set statusbar visible of targetWindow to false
                set the bounds of targetWindow to {{200, 120, 860, 520}}
                
                set viewOptions to the icon view options of targetWindow
                set arrangement of viewOptions to not arranged
                set icon size of viewOptions to 100
                delay 0.5
                
                -- 设置图标位置（相对于文件夹）
                try
                    set position of item "{}" of dmgPath to {{170, 190}}
                on error errMsg
                    log "设置应用图标位置失败: " & errMsg
                end try
                try
                    set position of item "{}" of dmgPath to {{170, 190}}
                on error errMsg
                    log "设置应用图标位置（备用）失败: " & errMsg
                end try
                delay 0.5
                try
                    set position of item "Applications" of dmgPath to {{490, 190}}
                on error errMsg
                    log "设置 Applications 图标位置失败: " & errMsg
                end try
                delay 1
                
                -- 强制 Finder 保存视图设置到 .DS_Store
                -- 方法1: 关闭并重新打开窗口
                close targetWindow
                delay 0.5
                open dmgPath
                delay 1
                
                -- 方法2: 使用 update 命令强制保存
                update dmgPath without registering applications
                delay 1
                
                -- 方法3: 再次关闭窗口，确保写入完成
                close (container window of dmgPath)
                delay 1
            end tell
            "#,
            mount_point,
            app_name,
            app_name.trim_end_matches(".app")
        );
        tracing::info!("📝 执行 AppleScript 设置图标位置...");
        let osascript_output = Command::new("osascript")
            .arg("-e")
            .arg(&applescript)
            .output()
            .context("Failed to execute osascript")?;
        
        if !osascript_output.status.success() {
            let stderr = String::from_utf8_lossy(&osascript_output.stderr);
            let stdout = String::from_utf8_lossy(&osascript_output.stdout);
            tracing::error!("❌ AppleScript 执行失败！");
            tracing::error!("   退出码: {:?}", osascript_output.status.code());
            tracing::error!("   标准错误: {}", stderr);
            if !stdout.is_empty() {
                tracing::error!("   标准输出: {}", stdout);
            }
            
            if stderr.contains("-1743") || stderr.contains("未获得授权") {
                tracing::warn!("⚠️  AppleScript 需要 Finder 自动化权限");
                tracing::warn!("⚠️  请打开 系统设置 → 隐私与安全性 → 自动化 → 终端 → 勾选 Finder");
            }
            return Err(anyhow::anyhow!("AppleScript 执行失败: {}", stderr));
        } else {
            let stdout = String::from_utf8_lossy(&osascript_output.stdout);
            if !stdout.is_empty() {
                tracing::info!("   AppleScript 输出: {}", stdout);
            }
            tracing::info!("✅ AppleScript 执行成功");
        }
        
        // 确保 Finder 关闭所有窗口
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(format!(r#"tell application "Finder" to close every window whose name contains "{}""#, 
                mount_point.split('/').last().unwrap_or("")))
            .output();
        
        // 等待 Finder 完成 .DS_Store 写入（Finder 会异步写入，需要足够时间）
        std::thread::sleep(std::time::Duration::from_secs(2));
        
        // 验证 .DS_Store 文件是否存在并输出详细信息
        let ds_store_path = format!("{}/.DS_Store", mount_point);
        let ds_store_file = std::path::Path::new(&ds_store_path);
        
        tracing::info!("🔍 检查 .DS_Store 文件:");
        tracing::info!("   路径: {}", ds_store_path);
        
        if ds_store_file.exists() {
            if let Ok(metadata) = std::fs::metadata(&ds_store_path) {
                tracing::info!("   ✅ 文件存在");
                tracing::info!("   大小: {} 字节", metadata.len());
                tracing::info!("   权限: {:?}", metadata.permissions());
            } else {
                tracing::warn!("   ⚠️  文件存在但无法读取元数据");
            }
        } else {
            tracing::warn!("   ❌ 文件不存在，等待更长时间...");
            std::thread::sleep(std::time::Duration::from_secs(2));
            
            // 再次检查
            if ds_store_file.exists() {
                if let Ok(metadata) = std::fs::metadata(&ds_store_path) {
                    tracing::info!("   ✅ 文件现在存在了");
                    tracing::info!("   大小: {} 字节", metadata.len());
                }
            } else {
                tracing::error!("   ❌ .DS_Store 文件仍然不存在！");
            }
        }
        
        // 列出挂载点下的所有文件（包括隐藏文件）
        tracing::info!("🔍 挂载点目录内容:");
        if let Ok(entries) = std::fs::read_dir(&mount_point) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let file_name = entry.file_name();
                    let file_name_str = file_name.to_string_lossy();
                    if let Ok(metadata) = entry.metadata() {
                        tracing::info!("   {} ({} 字节)", file_name_str, metadata.len());
                    }
                }
            }
        }
        
        // 强制同步磁盘，确保 .DS_Store 写入完成
        tracing::info!("💾 同步磁盘...");
        let _ = Command::new("sync").output();
        std::thread::sleep(std::time::Duration::from_millis(500));
        
        // 再次同步确保写入完成
        let _ = Command::new("sync").output();
        
        // 强制卸载 DMG
        let detach_result = Command::new("hdiutil")
            .arg("detach")
            .arg(&mount_point)
            .arg("-force")
            .output();
        
        if let Ok(output) = detach_result {
            if !output.status.success() {
                tracing::warn!("⚠️  首次卸载失败，重试...");
                std::thread::sleep(std::time::Duration::from_secs(1));
                let _ = Command::new("hdiutil")
                    .arg("detach")
                    .arg(&mount_point)
                    .arg("-force")
                    .output();
            }
        }
        
        // 等待系统完全释放资源
        std::thread::sleep(std::time::Duration::from_secs(1));
        
        Ok(())
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn set_dmg_icon_positions(&self, _dmg_path: &Path, _app_name: &str) -> Result<()> {
        Ok(())
    }
    
    /// 查找 pkg-dmg 工具
    #[cfg(target_os = "macos")]
    async fn find_pkg_dmg(&self, src_path: &Path, out_dir: &str) -> Result<std::path::PathBuf> {
        // 可能的路径
        let possible_paths = vec![
            src_path.join(out_dir).join("JoyME Packaging/pkg-dmg"),
            src_path.join(out_dir).join("chrome/installer/mac/pkg-dmg"),
            src_path.join(out_dir).join("pkg-dmg"),
        ];
        
        for path in possible_paths {
            if path.exists() {
                return Ok(path);
            }
        }
        
        Err(anyhow::anyhow!("找不到 pkg-dmg 工具"))
    }
    
    #[cfg(not(target_os = "macos"))]
    async fn find_pkg_dmg(&self, _src_path: &Path, _out_dir: &str) -> Result<std::path::PathBuf> {
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

