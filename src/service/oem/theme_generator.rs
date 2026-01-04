use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use crate::image::image_util;

/// 生成完整的主题资源，参考 Brave 的目录结构
pub struct ThemeGenerator {
    base_path: PathBuf,
    #[allow(dead_code)] // 保留用于未来扩展
    oem_name: String,
}

impl ThemeGenerator {
    pub fn new(base_path: &Path, oem_name: &str) -> Self {
        Self {
            base_path: base_path.to_path_buf(),
            oem_name: oem_name.to_string(),
        }
    }

    /// 生成所有主题资源
    pub async fn generate_all(&self, logo_path: &str, document_path: Option<&str>) -> Result<()> {
        // 确保基础目录存在
        tokio::fs::create_dir_all(&self.base_path)
            .await
            .context("Failed to create base theme directory")?;

        // 1. 生成 brave/ 目录下的资源
        self.generate_brave_resources(logo_path, document_path).await?;

        // 2. 生成 chromium/ 目录下的资源
        self.generate_chromium_resources(logo_path).await?;

        // 3. 生成 default_100_percent/brave/ 目录下的资源
        self.generate_default_100_percent_resources(logo_path).await?;

        // 4. 生成 default_200_percent/brave/ 目录下的资源
        self.generate_default_200_percent_resources(logo_path).await?;

        Ok(())
    }

    /// 生成 brave/ 目录下的资源
    async fn generate_brave_resources(&self, logo_path: &str, document_path: Option<&str>) -> Result<()> {
        let brave_dir = self.base_path.join("brave");
        
        // Linux 资源
        self.generate_linux_resources(&brave_dir, logo_path).await?;
        
        // macOS 资源
        self.generate_mac_resources(&brave_dir, logo_path, document_path).await?;
        
        // Windows 资源
        self.generate_windows_resources(&brave_dir, logo_path).await?;
        
        // Android 资源
        self.generate_android_resources(&brave_dir, logo_path).await?;
        
        // 根目录通用资源
        self.generate_brave_root_resources(&brave_dir, logo_path).await?;

        Ok(())
    }

    /// 生成多个尺寸的图片（通用辅助函数）
    async fn generate_sized_images(
        &self,
        output_dir: &Path,
        logo_path: &str,
        sizes: &[u32],
        filename_prefix: &str,
    ) -> Result<()> {
        tokio::fs::create_dir_all(output_dir).await?;
        
        sizes.iter().for_each(|&size| {
            let filename = format!("{}_{}.png", filename_prefix, size);
            let output_path = output_dir.join(&filename);
            if let Some(output_str) = output_path.to_str() {
                image_util::resize_image_with_scaler(
                    logo_path,
                    Some(output_str),
                    size,
                    size,
                );
            }
        });
        
        Ok(())
    }

    /// 使用临时文件生成资源（通用辅助函数）
    async fn with_temp_file(
        &self,
        temp_dir: &Path,
        temp_filename: &str,
        logo_path: &str,
        resize_size: Option<u32>,
    ) -> Result<PathBuf> {
        let temp_file_path = temp_dir.join(temp_filename);
        tokio::fs::create_dir_all(temp_dir).await?;
        
        let saved = resize_size
            .and_then(|size| image_util::resize_image_with_scaler(logo_path, None, size, size))
            .map(|resized| resized.save(&temp_file_path).context("Failed to save temp logo"))
            .transpose()?;
        
        if saved.is_none() {
            tokio::fs::copy(logo_path, &temp_file_path).await?;
        }
        
        Ok(temp_file_path)
    }

    /// 生成 Linux 平台资源
    async fn generate_linux_resources(&self, brave_dir: &Path, logo_path: &str) -> Result<()> {
        let linux_dir = brave_dir.join("linux");
        let sizes = vec![16, 24, 32, 48, 64, 128, 256];
        self.generate_sized_images(&linux_dir, logo_path, &sizes, "product_logo").await?;

        // 生成 XPM 格式 (32x32) - 先跳过，需要特殊转换工具
        // let xpm_path = linux_dir.join("product_logo_32.xpm");

        Ok(())
    }

    /// 生成 macOS 平台资源
    async fn generate_mac_resources(&self, brave_dir: &Path, logo_path: &str, document_path: Option<&str>) -> Result<()> {
        let mac_dir = brave_dir.join("mac");
        tokio::fs::create_dir_all(&mac_dir).await?;

        // 生成 app.icns
        let temp_logo_path = self.with_temp_file(
            &mac_dir,
            "temp_logo_for_icns.png",
            logo_path,
            Some(512),
        ).await?;
        
        if let Some(temp_logo_str) = temp_logo_path.to_str() {
            image_util::generate_chromium_icns(temp_logo_str, "app.icns", true);
        }
        
        let _ = tokio::fs::remove_file(&temp_logo_path).await;

        // 生成 document.icns (如果有文档图)
        if let Some(doc_path) = document_path {
            // 先确保 product_logo_192.png 存在（在 logo_dir 中）
            let logo_dir = Path::new(logo_path).parent().unwrap();
            let logo_192_path = logo_dir.join("product_logo_192.png");
            let logo_192_created = !logo_192_path.exists();
            
            if logo_192_created {
                if let Some(logo_192_str) = logo_192_path.to_str() {
                    image_util::resize_image_with_scaler(
                        logo_path,
                        Some(logo_192_str),
                        192,
                        192,
                    );
                }
            }
            
            // 在目标目录创建临时文件用于生成 document.icns
            let temp_doc_path = self.with_temp_file(
                &mac_dir,
                "temp_document_for_icns.png",
                doc_path,
                None,
            ).await?;
            
            if let Some(temp_doc_str) = temp_doc_path.to_str() {
                image_util::generate_chromium_document_icns(temp_doc_str, "document.icns");
            }
            
            let _ = tokio::fs::remove_file(&temp_doc_path).await;
            
            // 清理临时创建的 product_logo_192.png（如果是我们创建的）
            if logo_192_created && logo_192_path.exists() {
                let _ = tokio::fs::remove_file(&logo_192_path).await;
            }
        }

        Ok(())
    }

    /// 生成 Windows 平台资源
    async fn generate_windows_resources(&self, brave_dir: &Path, logo_path: &str) -> Result<()> {
        let win_dir = brave_dir.join("win");
        tokio::fs::create_dir_all(&win_dir).await?;

        // 生成各种 ico 文件
        let temp_logo_win = self.with_temp_file(
            &win_dir,
            "temp_logo_for_ico.png",
            logo_path,
            Some(256),
        ).await?;
        
        if let Some(temp_logo_str) = temp_logo_win.to_str() {
            ["brave.ico", "app_list.ico", "app_list_sxs.ico", "incognito.ico"]
                .iter()
                .for_each(|&ico_name| {
                    image_util::generate_chromium_ico(temp_logo_str, ico_name);
                });
        }
        
        let _ = tokio::fs::remove_file(&temp_logo_win).await;

        // 生成 tiles
        let tiles_dir = win_dir.join("tiles");
        let temp_logo_tiles = self.with_temp_file(
            &tiles_dir,
            "temp_logo_for_tiles.png",
            logo_path,
            Some(256),
        ).await?;
        
        if let Some(temp_logo_str) = temp_logo_tiles.to_str() {
            image_util::generate_chromium_logo(temp_logo_str, "Logo.png", 600, 188);
            image_util::generate_chromium_logo(temp_logo_str, "SmallLogo.png", 176, 24);
        }
        
        let _ = tokio::fs::remove_file(&temp_logo_tiles).await;

        Ok(())
    }

    /// 生成 Android 平台资源
    async fn generate_android_resources(&self, brave_dir: &Path, logo_path: &str) -> Result<()> {
        let android_dir = brave_dir.join("android");
        
        // mipmap 和 drawable 的尺寸配置
        let android_sizes = [
            ("mipmap-mdpi", 48),
            ("mipmap-hdpi", 72),
            ("mipmap-xhdpi", 96),
            ("mipmap-xxhdpi", 144),
            ("mipmap-xxxhdpi", 192),
        ];

        // 生成 mipmap 资源
        for (dir_name, size) in android_sizes.iter() {
            let mipmap_dir = android_dir.join(dir_name);
            tokio::fs::create_dir_all(&mipmap_dir).await?;
            mipmap_dir
                .join("app_icon.png")
                .to_str()
                .map(|app_icon_str| {
                    image_util::resize_image_with_scaler(logo_path, Some(app_icon_str), *size, *size);
                });
        }

        // 生成 drawable 资源
        let res_brave_dir = android_dir.join("res_brave");
        let drawable_sizes = android_sizes.iter().map(|(dir, size)| {
            (dir.replace("mipmap", "drawable"), *size)
        });
        
        for (dir_name, size) in drawable_sizes {
            let drawable_dir = res_brave_dir.join(&dir_name);
            tokio::fs::create_dir_all(&drawable_dir).await?;
            drawable_dir
                .join("fre_product_logo.png")
                .to_str()
                .map(|logo_file_str| {
                    image_util::resize_image_with_scaler(logo_path, Some(logo_file_str), size, size);
                });
        }

        Ok(())
    }

    /// 生成 brave/ 根目录下的通用资源
    async fn generate_brave_root_resources(&self, brave_dir: &Path, logo_path: &str) -> Result<()> {
        // 生成各种尺寸的 product_logo
        let sizes = vec![16, 22, 24, 48, 64, 128, 256];
        self.generate_sized_images(brave_dir, logo_path, &sizes, "product_logo").await?;

        // 生成灰度版本
        let mono_path = brave_dir.join("product_logo_22_mono.png");
        if let Some(mono_str) = mono_path.to_str() {
            image_util::generate_grayscale_image(logo_path, mono_str, 22);
        }

        // 生成 SVG (如果原图是 SVG，直接复制；否则需要转换)
        if logo_path.ends_with(".svg") {
            let svg_path = brave_dir.join("product_logo.svg");
            tokio::fs::copy(logo_path, &svg_path).await?;
        }

        Ok(())
    }

    /// 生成 chromium/ 目录下的资源
    async fn generate_chromium_resources(&self, logo_path: &str) -> Result<()> {
        let chromium_dir = self.base_path.join("chromium");
        tokio::fs::create_dir_all(&chromium_dir).await?;

        // Linux 资源
        let linux_dir = chromium_dir.join("linux");
        let sizes = vec![24, 32, 48, 64, 128, 256];
        self.generate_sized_images(&linux_dir, logo_path, &sizes, "product_logo").await?;

        // macOS 资源
        let mac_dir = chromium_dir.join("mac");
        let temp_logo_chromium = self.with_temp_file(
            &mac_dir,
            "temp_logo_for_icns.png",
            logo_path,
            Some(512),
        ).await?;
        
        if let Some(temp_logo_str) = temp_logo_chromium.to_str() {
            image_util::generate_chromium_icns(temp_logo_str, "app.icns", true);
        }
        
        let _ = tokio::fs::remove_file(&temp_logo_chromium).await;

        // 根目录资源
        let root_sizes = vec![16, 24, 48, 64, 128, 256];
        self.generate_sized_images(&chromium_dir, logo_path, &root_sizes, "product_logo").await?;

        // 生成灰度版本
        let mono_path = chromium_dir.join("product_logo_22_mono.png");
        if let Some(mono_str) = mono_path.to_str() {
            image_util::generate_grayscale_image(logo_path, mono_str, 22);
        }

        Ok(())
    }

    /// 生成 default_100_percent/brave/ 目录下的资源
    async fn generate_default_100_percent_resources(&self, logo_path: &str) -> Result<()> {
        let default_100_dir = self.base_path.join("default_100_percent").join("brave");
        tokio::fs::create_dir_all(&default_100_dir).await?;

        // Linux 资源
        let linux_dir = default_100_dir.join("linux");
        let sizes = vec![16, 32];
        self.generate_sized_images(&linux_dir, logo_path, &sizes, "product_logo").await?;

        // 根目录资源
        self.generate_sized_images(&default_100_dir, logo_path, &sizes, "product_logo").await?;

        // 生成 product_logo_name 系列
        for filename in &["product_logo_name_22.png", "product_logo_name_22_white.png"] {
            let path = default_100_dir.join(filename);
            if let Some(path_str) = path.to_str() {
                image_util::resize_image_with_scaler(logo_path, Some(path_str), 22, 22);
            }
        }

        Ok(())
    }

    /// 生成 default_200_percent/brave/ 目录下的资源
    async fn generate_default_200_percent_resources(&self, logo_path: &str) -> Result<()> {
        let default_200_dir = self.base_path.join("default_200_percent").join("brave");
        tokio::fs::create_dir_all(&default_200_dir).await?;

        // 根目录资源 (200% 缩放，所以尺寸是 100% 的两倍)
        let root_sizes = vec![32, 64]; // 对应 100% 的 16 和 32
        self.generate_sized_images(&default_200_dir, logo_path, &root_sizes, "product_logo").await?;

        // 生成 product_logo_name 系列 (200% 缩放)
        for filename in &["product_logo_name_22.png", "product_logo_name_22_white.png"] {
            let path = default_200_dir.join(filename);
            if let Some(path_str) = path.to_str() {
                image_util::resize_image_with_scaler(logo_path, Some(path_str), 44, 44);
            }
        }

        Ok(())
    }
}
