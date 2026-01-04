use std::path::{Path, PathBuf};
use crate::error::{AppError, AppResult};

#[allow(dead_code)]
pub fn validate_path(path: &Path, base: &Path) -> AppResult<PathBuf> {
    let canonical_base = base.canonicalize()
        .map_err(|_| AppError::InvalidPath(format!("Invalid base path: {:?}", base)))?;
    
    let canonical_path = path.canonicalize()
        .map_err(|_| AppError::InvalidPath(format!("Invalid path: {:?}", path)))?;
    
    if !canonical_path.starts_with(&canonical_base) {
        return Err(AppError::InvalidPath(
            format!("Path outside base directory: {:?}", path)
        ));
    }
    
    Ok(canonical_path)
}

#[allow(dead_code)]
pub fn sanitize_filename(filename: &str) -> String {
    filename
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect()
}

