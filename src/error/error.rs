use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(#[from] anyhow::Error),
    
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    
    #[error("Build error: {0}")]
    #[allow(dead_code)]
    Build(String),
    
    #[error("Task not found: {id}")]
    #[allow(dead_code)]
    TaskNotFound { id: i64 },
    
    #[error("Task already in progress")]
    #[allow(dead_code)]
    TaskInProgress,
    
    #[error("Invalid path: {0}")]
    #[allow(dead_code)]
    InvalidPath(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Validation error: {0}")]
    Validation(String),
    
    #[error("Git error: {0}")]
    #[allow(dead_code)]
    Git(String),
    
    #[error("Command execution error: {0}")]
    #[allow(dead_code)]
    Command(String),
}

impl From<validator::ValidationErrors> for AppError {
    fn from(err: validator::ValidationErrors) -> Self {
        AppError::Validation(format!("{:?}", err))
    }
}

pub type AppResult<T> = Result<T, AppError>;

