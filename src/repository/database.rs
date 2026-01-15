use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Executor, SqlitePool};
use std::path::PathBuf;
use anyhow::{Context, Result};
use crate::config::AppConfig;

pub async fn init_db(config: &AppConfig) -> Result<Option<SqlitePool>> {
    let db_name = config.get_db_path();
    
    if db_name.is_empty() {
        tracing::info!("build machine, no database");
        return Ok(None);
    }
    
    let mut database_path = PathBuf::from(std::env::current_dir()?);
    database_path.push(db_name);
    
    tracing::info!("database path: {:?}", database_path);
    
    if !database_path.exists() {
        std::fs::File::create(&database_path)
            .context("Failed to create database file")?;
    }
    
    let database_url = format!(
        "sqlite://{}",
        database_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid database path"))?
    );
    
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("Failed to connect to database")?;
    
    // 创建表
    pool.execute(
        r#"
        CREATE TABLE IF NOT EXISTS pkg (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            start_time TEXT NOT NULL,
            end_time TEXT,
            branch_name TEXT NOT NULL,
            oem_name TEXT,
            commit_id TEXT,
            pkg_flag TEXT,
            is_signed BOOLEAN,
            is_increment BOOLEAN,
            storage_path TEXT,
            installer TEXT,
            state TEXT,
            server TEXT,
            parent_id INTEGER,
            architecture TEXT
        );
        "#,
    )
    .await
    .context("Failed to create table")?;
    
    // 添加新字段（如果表已存在，这些操作会失败但不会影响功能）
    let _ = pool.execute("ALTER TABLE pkg ADD COLUMN parent_id INTEGER").await;
    let _ = pool.execute("ALTER TABLE pkg ADD COLUMN architecture TEXT").await;
    let _ = pool.execute("ALTER TABLE pkg ADD COLUMN build_log TEXT").await;
    let _ = pool.execute("ALTER TABLE pkg ADD COLUMN installer_format TEXT").await;
    
    Ok(Some(pool))
}

