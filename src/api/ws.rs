use axum::{
    extract::{ws::WebSocketUpgrade, State, Path as AxumPath},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast;
use crate::api::AppState;
use crate::repository::task::TaskRepository;
use crate::util::time;
use tracing::{info, warn, error};

/// WebSocket 消息类型
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogMessage {
    pub task_id: i64,
    pub log: String,
    pub timestamp: String,
    #[serde(default)]
    pub is_progress: bool,  // 是否为进度行（需要刷新同一行）
}

/// WebSocket 连接管理器
#[derive(Debug, Clone)]
pub struct WsManager {
    /// 每个任务 ID 对应一个广播通道
    channels: Arc<dashmap::DashMap<i64, broadcast::Sender<LogMessage>>>,
}

impl WsManager {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(dashmap::DashMap::new()),
        }
    }
    
    /// 获取或创建任务的广播通道
    fn get_or_create_channel(&self, task_id: i64) -> broadcast::Sender<LogMessage> {
        self.channels
            .entry(task_id)
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(1000); // 缓冲区大小 1000
                tx
            })
            .clone()
    }
    
    /// 广播日志消息到所有订阅该任务的客户端
    pub fn broadcast_log(&self, task_id: i64, log: String, is_progress: bool) {
        let channel = self.get_or_create_channel(task_id);
        let timestamp = time::format_date_time().unwrap_or_else(|_| "N/A".to_string());
        let message = LogMessage {
            task_id,
            log,
            timestamp,
            is_progress,
        };
        
        // 忽略错误（如果没有订阅者，这是正常的）
        let _ = channel.send(message);
    }
    
    /// 订阅任务的日志流
    pub fn subscribe(&self, task_id: i64) -> broadcast::Receiver<LogMessage> {
        self.get_or_create_channel(task_id).subscribe()
    }
    
    /// 清理不再需要的通道（可选，用于资源管理）
    #[allow(dead_code)]
    pub fn remove_channel(&self, task_id: i64) {
        self.channels.remove(&task_id);
    }
}

impl Default for WsManager {
    fn default() -> Self {
        Self::new()
    }
}

/// WebSocket handler：处理客户端连接
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    AxumPath(task_id): AxumPath<i64>,
) -> Response {
    // 验证任务是否存在
    let task_repo = match &state.db_pool {
        Some(pool) => TaskRepository::new(pool.clone()),
        None => {
            return axum::response::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body("Database not available".into())
                .unwrap();
        }
    };
    
    // 检查任务是否存在
    if task_repo.find_by_id(task_id).await.is_err() {
        return axum::response::Response::builder()
            .status(axum::http::StatusCode::NOT_FOUND)
            .body("Task not found".into())
            .unwrap();
    }
    
    let ws_manager = state.ws_manager.clone();
    let db_pool = state.db_pool.clone();
    
    ws.on_upgrade(move |socket| handle_socket(socket, task_id, ws_manager, db_pool))
}

/// 处理 WebSocket 连接
async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    task_id: i64,
    ws_manager: WsManager,
    db_pool: Option<sqlx::SqlitePool>,
) {
    let (mut sender, mut receiver) = socket.split();
    
    // 订阅任务的日志流
    let mut rx = ws_manager.subscribe(task_id);
    
    info!("WebSocket 客户端已连接，任务 ID: {}", task_id);
    
    // 发送历史日志（如果有）
    if let Some(pool) = db_pool {
        let task_repo = TaskRepository::new(pool);
        if let Ok(Some(log)) = task_repo.get_build_log(task_id).await {
            if !log.is_empty() {
                // 发送历史日志
                let timestamp = time::format_date_time().unwrap_or_else(|_| "N/A".to_string());
                let message = LogMessage {
                    task_id,
                    log: log.clone(),
                    timestamp,
                    is_progress: false,
                };
                if let Ok(json) = serde_json::to_string(&message) {
                    if let Err(e) = sender.send(axum::extract::ws::Message::Text(json)).await {
                        warn!("发送历史日志失败: {:?}", e);
                    }
                }
            }
        }
    }
    
    // 使用 channel 来处理 Ping/Pong
    let (pong_tx, mut pong_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(10);
    
    // 启动发送任务：从广播通道接收日志并发送给客户端，同时处理 Pong
    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // 接收日志消息
                msg = rx.recv() => {
                    match msg {
                        Ok(log_msg) => {
                            let json = match serde_json::to_string(&log_msg) {
                                Ok(json) => json,
                                Err(e) => {
                                    error!("序列化日志消息失败: {:?}", e);
                                    continue;
                                }
                            };
                            
                            if let Err(e) = sender.send(axum::extract::ws::Message::Text(json)).await {
                                warn!("发送 WebSocket 消息失败: {:?}", e);
                                break;
                            }
                        }
                        Err(_) => {
                            // 通道关闭
                            break;
                        }
                    }
                }
                // 处理 Pong 消息
                Some(data) = pong_rx.recv() => {
                    if let Err(e) = sender.send(axum::extract::ws::Message::Pong(data)).await {
                        warn!("发送 Pong 失败: {:?}", e);
                        break;
                    }
                }
            }
        }
    });
    
    // 启动接收任务：接收客户端消息（用于心跳检测）
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                axum::extract::ws::Message::Close(_) => {
                    info!("WebSocket 客户端断开连接，任务 ID: {}", task_id);
                    break;
                }
                axum::extract::ws::Message::Ping(data) => {
                    // 发送 Pong 响应
                    if let Err(_) = pong_tx.send(data).await {
                        break;
                    }
                }
                _ => {
                    // 忽略其他消息
                }
            }
        }
    });
    
    // 等待任一任务完成
    tokio::select! {
        _ = &mut send_task => {
            info!("发送任务完成，任务 ID: {}", task_id);
        }
        _ = &mut recv_task => {
            info!("接收任务完成，任务 ID: {}", task_id);
        }
    }
}

