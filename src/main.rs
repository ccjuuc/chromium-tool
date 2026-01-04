mod api;
mod config;
mod error;
mod model;
mod repository;
mod service;
mod util;
mod image;  // 图像处理工具

use anyhow::Result;
use tokio::net::TcpListener;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, fmt, Layer};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    // 创建日志目录
    let log_dir = PathBuf::from("logs");
    if !log_dir.exists() {
        std::fs::create_dir_all(&log_dir)?;
    }
    
    // 配置日志文件（按日期滚动）
    let file_appender = tracing_appender::rolling::daily(&log_dir, "chromium_tool.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    
    // 环境过滤器
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    
    // 控制台输出层（带颜色）
    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_filter(filter.clone());
    
    // 文件输出层（无颜色）
    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(filter);
    
    // 初始化日志：同时输出到控制台和文件
    tracing_subscriber::registry()
        .with(console_layer)
        .with(file_layer)
        .init();
    
    // 保持 _guard 存活（防止日志丢失）
    // 注意：在实际应用中，可能需要将 _guard 存储在某个地方以保持其生命周期
    tracing::info!("日志文件位置: {}/chromium_tool.log", log_dir.display());
    
    // 加载配置
    let config = config::AppConfig::load("config.toml").await?;
    
    // 初始化数据库
    let db_pool = repository::database::init_db(&config).await?;
    
    // 重置异常终止的任务状态
    if let Some(pool) = &db_pool {
        if let Ok(count) = repository::task::TaskRepository::reset_running_tasks(pool).await {
            if count > 0 {
                tracing::warn!("⚠️  发现 {} 个异常终止的任务，已重置为 failed", count);
            }
        }
    }
    
    // 构建应用状态
    let app_state = api::AppState::new(config, db_pool);
    
    // 配置路由
    let app = api::routes::create_router(app_state);
    
    // 启动服务器
    let listener = TcpListener::bind("0.0.0.0:3000").await?;
    tracing::info!("Server listening on 0.0.0.0:3000");
    axum::serve(listener, app).await?;
    
    Ok(())
}