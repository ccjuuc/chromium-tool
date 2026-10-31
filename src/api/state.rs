use std::sync::Arc;
use sqlx::SqlitePool;
use dashmap::DashMap;
use tokio::sync::Mutex;
use crate::config::AppConfig;
use crate::service::task::TaskService;
use crate::service::build::BuildService;
use crate::repository::task::TaskRepository;
use crate::api::ws::WsManager;

#[derive(Clone)]
pub struct AppState {
    pub db_pool: Option<SqlitePool>,
    pub config: Arc<AppConfig>,
    pub task_service: Option<Arc<TaskService>>,
    pub build_service: Option<Arc<BuildService>>,
    pub task_repo: Option<Arc<TaskRepository>>,
    pub ws_manager: WsManager,
    // 按服务器分组的锁，防止同一服务器并发创建任务
    pub server_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("db_pool", &self.db_pool.is_some())
            .field("config", &"AppConfig")
            .field("task_service", &self.task_service.is_some())
            .field("build_service", &self.build_service.is_some())
            .field("task_repo", &self.task_repo.is_some())
            .field("ws_manager", &"WsManager")
            .field("server_locks", &format!("DashMap with {} entries", self.server_locks.len()))
            .finish()
    }
}

impl AppState {
    pub fn new(config: AppConfig, db_pool: Option<SqlitePool>) -> Self {
        let config_arc = Arc::new(config.clone());
        let ws_manager = WsManager::new();
        
        let (task_service, task_repo) = db_pool.as_ref().map(|pool| {
            let repo = TaskRepository::new(pool.clone());
            let repo_arc = Arc::new(repo.clone());
            let service = Arc::new(TaskService::new(repo));
            (Some(service), Some(repo_arc))
        }).unwrap_or((None, None));
        
        let build_service = Some(Arc::new(
            BuildService::new(config.clone())
                .with_ws_manager(ws_manager.clone())
        ));
        
        Self {
            db_pool,
            config: config_arc,
            task_service,
            build_service,
            task_repo,
            ws_manager,
            server_locks: Arc::new(DashMap::new()),
        }
    }
    
    /// 获取指定服务器的锁，防止并发创建任务
    /// 返回 Arc<Mutex<()>>，调用者需要获取 guard
    pub fn get_server_lock(&self, server: &str) -> Arc<Mutex<()>> {
        self.server_locks
            .entry(server.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
    
    /// 根据服务器地址推断所属平台
    pub fn infer_platform(&self, server: &str) -> String {
        if self.config.server.macos.iter().any(|s| s == server) || server.contains("macos") {
            "macos".to_string()
        } else if self.config.server.windows.iter().any(|s| s == server) || server.contains("windows") {
            "windows".to_string()
        } else if self.config.server.linux.iter().any(|s| s == server) || server.contains("linux") {
            "linux".to_string()
        } else {
            std::env::consts::OS.to_string()
        }
    }

    /// 启动下一个 pending 任务（用于任务完成后的自动排队）
    pub fn start_next_pending_task(self: Arc<Self>, server: String) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            // 获取服务器锁，避免并发唤醒同一个排队任务
            let server_lock = self.get_server_lock(&server);
            let _guard = server_lock.lock().await;

            let task_repo = match &self.task_repo {
                Some(repo) => repo.clone(),
                None => return,
            };
            
            let build_service = match &self.build_service {
                Some(service) => service.clone(),
                None => return,
            };
            
            let task_service = match &self.task_service {
                Some(service) => service.clone(),
                None => return,
            };

            // 如果该服务器上仍有正在运行的任务，则不重复启动
            if task_repo.has_running_task_on_server(&server).await.unwrap_or(false) {
                return;
            }
            
            // 检查下一个 pending 任务（优先查找子任务，如果没有则查找单架构任务）
            let next_task_id = match task_repo.get_next_pending_child_task_on_server(server.as_str()).await {
                Ok(Some(id)) => Some(id),
                Ok(None) => {
                    // 如果没有子任务，查找单架构任务（parent_id IS NULL 且 architecture IS NOT NULL）
                    task_repo.get_next_pending_task_on_server(server.as_str()).await.ok().flatten()
                },
                Err(_) => None,
            };
            
            if let Some(next_task_id) = next_task_id {
                tracing::info!("启动下一个排队任务 #{}", next_task_id);
            
                // 获取任务信息，检查任务状态
                match task_repo.find_by_id(next_task_id).await {
                    Ok(next_task) => {
                        // 检查任务状态，如果已经被删除、标记为失败或取消，不启动
                        if matches!(next_task.state, crate::model::state::TaskState::Failed | crate::model::state::TaskState::Cancelled) {
                            tracing::warn!("任务 #{} 已被标记为失败或取消，跳过启动", next_task_id);
                            return;
                        }
                        if let Some(arch) = &next_task.architecture {
                            // 优先从持久化的 request_json 恢复完整构建参数（保留 custom_args、is_update、emails、platform 等）
                            let request = next_task
                                .request_json
                                .as_deref()
                                .and_then(|json| serde_json::from_str::<crate::model::build::BuildRequest>(json).ok())
                                .unwrap_or_else(|| crate::model::build::BuildRequest {
                                    branch: next_task.branch_name.clone(),
                                    commit_id: if next_task.commit_id.is_empty() { None } else { Some(next_task.commit_id) },
                                    pkg_flag: next_task.pkg_flag.clone(),
                                    installer_format: next_task.installer_format.clone(),
                                    is_increment: next_task.is_increment,
                                    is_signed: next_task.is_signed,
                                    server: next_task.server.clone(),
                                    platform: self.infer_platform(&next_task.server),
                                    architectures: vec![arch.clone()],
                                    is_x64: arch == "x64" || arch == "x86",
                                    custom_args: None,
                                    is_update: false,
                                    emails: None,
                                });
                            
                            // 在调用前克隆所有需要的值，确保 Send
                            let task_manager = task_service.manager().clone();
                            let build_service_clone = build_service.clone();
                            let task_repo_clone = task_repo.clone();
                            let app_state_clone = self.clone();
                            
                            // 使用 tokio::spawn 异步启动任务，避免阻塞
                            tokio::spawn(async move {
                                if let Err(e) = build_service_clone.start_pending_task(
                                    next_task_id,
                                    request,
                                    task_manager,
                                    task_repo_clone,
                                    Some(app_state_clone),
                                ).await {
                                    tracing::error!("启动下一个排队任务 #{} 失败: {:?}", next_task_id, e);
                                }
                            });
                        } else {
                            tracing::warn!("⚠️  任务 #{} 没有架构信息，跳过启动", next_task_id);
                        }
                    },
                    Err(e) => {
                        tracing::warn!("⚠️  无法获取任务 #{} 的信息: {}，可能已被删除，跳过启动", next_task_id, e);
                        eprintln!("⚠️  无法获取任务 #{} 的信息: {}，可能已被删除，跳过启动", next_task_id, e);
                        return;
                    }
                }
            }
        })
    }
}

