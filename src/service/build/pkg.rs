use std::path::Path;
use anyhow::{Result, Context};
use crate::service::build::installer::InstallerBuilder;
use tokio::fs;
use std::process::Command;

#[cfg(target_os = "macos")]
pub async fn create(builder: &InstallerBuilder, src_path: &Path, out_dir: &str) -> Result<()> {
    tracing::info!("📦 开始创建 PKG 安装包...");
    
    // 查找 .app 文件
    let app_name = builder.find_app_name(src_path, out_dir).await?;
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
    let pkg_name = generate_name(builder, src_path, out_dir, &app_name).await?;
    let pkg_path = output_dir.join(&pkg_name);
    
    // 使用 pkgbuild 创建 PKG
    tracing::info!("使用 pkgbuild 创建 PKG...");
    let base_name = app_name.trim_end_matches(".app");
    
    // 获取版本号
    let version = builder.read_version_from_info_plist(src_path, out_dir, &app_name).await
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
    let bundle_id = builder.read_bundle_id_from_info_plist(src_path, out_dir, &app_name).await
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

#[cfg(target_os = "macos")]
async fn generate_name(builder: &InstallerBuilder, src_path: &Path, out_dir: &str, app_name: &str) -> Result<String> {
    // 从 app_name 提取基础名称（去掉 .app）
    let base_name = app_name.trim_end_matches(".app");
    
    // 尝试从 Info.plist 读取版本号
    let version = if let Ok(version) = builder.read_version_from_info_plist(src_path, out_dir, app_name).await {
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
