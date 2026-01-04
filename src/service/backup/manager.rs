use std::path::Path;
use anyhow::Result;
use crate::config::AppConfig;
use crate::util::{hash, time};
use tokio::fs;
use walkdir::WalkDir;

#[derive(Clone)]
pub struct BackupManager {
    #[allow(dead_code)]
    pub(crate) config: AppConfig,
}

impl BackupManager {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }
    
    #[allow(dead_code)]
    pub async fn backup_files(
        &self,
        src_path: &Path,
        oem: &str,
        installer_files: &[(String, String)],  // (path, md5)
    ) -> Result<String> {
        let backup_base = Path::new(self.config.get_backup_path()?);
        
        // 创建日期目录
        let date_subfolder = time::format_date_folder()?;
        let date_dir = backup_base.join(&date_subfolder);
        fs::create_dir_all(&date_dir).await?;
        
        // 复制安装包
        for (installer_path, _md5) in installer_files {
            if let Some(filename) = Path::new(installer_path).file_name() {
                let dst = date_dir.join(filename);
                fs::copy(installer_path, &dst).await?;
            }
        }
        
        // 复制调试文件
        if !oem.is_empty() {
            let backup_subfolder = date_dir.join(oem);
            self.copy_debug_files(src_path, &backup_subfolder, oem).await?;
        }
        
        Ok(date_dir.to_string_lossy().to_string())
    }
    
    #[allow(dead_code)]
    async fn copy_debug_files(
        &self,
        data_dir: &Path,
        backup_dir: &Path,
        oem: &str,
    ) -> Result<()> {
        if !backup_dir.exists() {
            fs::create_dir_all(&backup_dir).await?;
        }
        
        for entry in WalkDir::new(data_dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let file_name_lower = file_name.to_lowercase();
            
            if !file_name_lower.contains(oem) {
                continue;
            }
            
            if file_name_lower.ends_with(".pdb")
                || file_name_lower.ends_with(".dbg")
                || file_name_lower.ends_with(".debug")
                || file_name_lower.ends_with(".dsym")
            {
                if entry.file_type().is_file() {
                    fs::copy(entry.path(), backup_dir.join(&file_name)).await?;
                } else if entry.file_type().is_dir() {
                    // 递归复制目录
                    self.copy_dir_recursive(entry.path(), &backup_dir.join(&file_name)).await?;
                }
            }
        }
        
        Ok(())
    }
    
    #[allow(dead_code)]
    async fn copy_dir_recursive(&self, src: &Path, dst: &Path) -> Result<()> {
        use std::collections::VecDeque;
        
        // 使用栈来模拟递归，避免递归调用
        let mut stack = VecDeque::new();
        stack.push_back((src.to_path_buf(), dst.to_path_buf()));
        
        while let Some((src_path, dst_path)) = stack.pop_back() {
            // 确保目标目录存在
            if !dst_path.exists() {
                fs::create_dir_all(&dst_path).await
                    .map_err(|e| anyhow::anyhow!("Failed to create directory {:?}: {}", dst_path, e))?;
            }
            
            // 读取源目录的所有条目
            let mut entries = fs::read_dir(&src_path).await
                .map_err(|e| anyhow::anyhow!("Failed to read directory {:?}: {}", src_path, e))?;
            
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
                        .map_err(|e| anyhow::anyhow!(
                            "Failed to copy file from {:?} to {:?}: {}",
                            entry_path, entry_dst, e
                        ))?;
                }
            }
        }
        
        Ok(())
    }
    
    #[allow(dead_code)]
    pub async fn calculate_installer_hash(&self, pkg_path: &str, extension: &str) -> Result<(String, String)> {
        use std::time::SystemTime;
        use regex::Regex;
        
        let mut installer_file = String::new();
        let mut last_file_tm = SystemTime::UNIX_EPOCH;
        let mut md5 = String::new();
        
        if Path::new(pkg_path).is_dir() {
            let version_regex = Regex::new(r"\d+\.\d+\.\d+\.\d+")?;
            
            for entry in WalkDir::new(pkg_path)
                .max_depth(1)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file() {
                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        if version_regex.is_match(file_name) {
                            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                                if extension == ext {
                                    let file_tm = entry.metadata()?.modified()?;
                                    if file_tm > last_file_tm {
                                        if installer_file.is_empty() 
                                            || (!installer_file.contains("old") && !installer_file.contains("bak")) {
                                            installer_file = path.to_string_lossy().to_string();
                                            last_file_tm = file_tm;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else {
            installer_file = pkg_path.to_string();
        }
        
        if Path::new(&installer_file).exists() && Path::new(&installer_file).is_file() {
            md5 = hash::calculate_file_hash(Path::new(&installer_file)).await?;
        }
        
        Ok((installer_file, md5))
    }
}

