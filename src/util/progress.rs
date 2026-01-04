use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use tokio::fs;

#[allow(dead_code)]
pub fn create_progress_bar(total: u64, message: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(&format!(
                "{{spinner:.green}} [{{elapsed_precise}}] [{{bar:40.cyan/blue}}] {{bytes}}/{{total_bytes}} ({{eta}}) {}",
                message
            ))
            .expect("Failed to create progress bar style")
            .progress_chars("#>-"),
    );
    pb
}

#[allow(dead_code)]
pub async fn copy_dir_with_progress(
    src: &Path,
    dst: &Path,
    pb: &ProgressBar,
) -> anyhow::Result<()> {
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
                // 复制文件并更新进度
                let metadata = fs::metadata(&entry_path).await
                    .map_err(|e| anyhow::anyhow!("Failed to get metadata for {:?}: {}", entry_path, e))?;
                let size = metadata.len();
                
                fs::copy(&entry_path, &entry_dst).await
                    .map_err(|e| anyhow::anyhow!(
                        "Failed to copy file from {:?} to {:?}: {}",
                        entry_path, entry_dst, e
                    ))?;
                
                pb.inc(size);
            }
        }
    }
    
    Ok(())
}

