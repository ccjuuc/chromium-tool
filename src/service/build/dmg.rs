use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use appdmg_rs::{DmgConfig, DmgContent, DmgWindow, DmgWindowSize};
use crate::service::build::installer::InstallerBuilder;

#[cfg(target_os = "macos")]
pub async fn create(builder: &InstallerBuilder, src_path: &Path, out_dir: &str) -> Result<()> {
    use tokio::fs;

    tracing::info!("📦 开始创建 DMG 安装包...");

    let app_name = builder.find_app_name(src_path, out_dir).await?;
    let app_path = src_path.join(out_dir).join(&app_name);

    if !app_path.exists() { return Err(anyhow::anyhow!("App not found: {}", app_path.display())); }

    let output_dir = src_path.join(out_dir).join("signed");
    fs::create_dir_all(&output_dir).await?;

    let dmg_name = generate_name(builder, src_path, out_dir, &app_name).await?;
    let final_dmg_path = output_dir.join(&dmg_name);
    if final_dmg_path.exists() { fs::remove_file(&final_dmg_path).await?; }

    // --- 准备配置 ---
    // 1. 创建临时目录存放背景图
    let temp_dir = std::env::temp_dir().join(format!("joyme_config_{}", std::process::id()));
    if temp_dir.exists() { fs::remove_dir_all(&temp_dir).await?; }
    fs::create_dir_all(&temp_dir).await?;

    let background_path = temp_dir.join("background.png");
    create_background(&background_path)?;

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
    let src_dir = Path::new(&builder.config.src.macos);
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
    appdmg_rs::build(&config, &final_dmg_path).await?;

    // 清理临时文件
    let _ = fs::remove_dir_all(&temp_dir).await;

    Ok(())
}

#[cfg(target_os = "macos")]
async fn generate_name(builder: &InstallerBuilder, src_path: &Path, out_dir: &str, app_name: &str) -> Result<String> {
    let base_name = app_name.trim_end_matches(".app");
    let version = if let Ok(version) = builder.read_version_from_info_plist(src_path, out_dir, app_name).await {
        version
    } else {
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        format!("{}", timestamp)
    };
    Ok(format!("{}-{}.dmg", base_name, version))
}

#[cfg(target_os = "macos")]
fn create_background(out_path: &Path) -> Result<()> {
    use image::{Rgba, RgbaImage};
    let width = 660u32;
    let height = 400u32;
    let mut img = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));

    let arrow_paths = vec![
        std::path::PathBuf::from("resources/dmg_arrow.png"), 
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
