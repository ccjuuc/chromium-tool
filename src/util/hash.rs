use ring::digest;
use hex;
use std::path::Path;
use anyhow::Result;
use tokio::fs::File;
use tokio::io::AsyncReadExt;

#[allow(dead_code)]
pub async fn calculate_file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).await?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await?;
    
    let hash = digest::digest(&digest::SHA256, &buffer);
    Ok(hex::encode(hash.as_ref()))
}

#[allow(dead_code)]
pub async fn calculate_file_hash_md5(path: &Path) -> Result<String> {
    // 如果需要 MD5 兼容性，可以使用 md-5 crate
    // 这里使用 SHA256 作为默认
    calculate_file_hash(path).await
}

