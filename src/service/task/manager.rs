use dashmap::DashMap;
use tokio::sync::Semaphore;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use crate::model::state::TaskState;

#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<DashMap<i64, TaskHandle>>,
    semaphore: Arc<Semaphore>,
}

struct TaskHandle {
    state: TaskState,
    handle: Option<tokio::task::JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
}

impl TaskManager {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            tasks: Arc::new(DashMap::new()),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }
    
    pub async fn start_task<F>(&self, task_id: i64, cancelled_flag: Arc<AtomicBool>, f: F) -> anyhow::Result<()>
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        // 在获取 permit 之前就更新/插入任务，确保 cancel_task 可以找到它
        if let Some(mut task) = self.tasks.get_mut(&task_id) {
            task.cancelled = cancelled_flag.clone();
        } else {
            self.tasks.insert(task_id, TaskHandle {
                state: TaskState::StartBuild,
                handle: None,
                cancelled: cancelled_flag.clone(),
            });
        }
        
        let semaphore_clone = self.semaphore.clone();
        let tasks_clone = self.tasks.clone();
        let handle = tokio::spawn(async move {
            // 在异步任务内部持有 OwnedSemaphorePermit，直到任务执行完毕才释放
            let _permit = match semaphore_clone.acquire_owned().await {
                Ok(permit) => permit,
                Err(e) => {
                    tracing::error!("Failed to acquire semaphore permit for task {}: {:?}", task_id, e);
                    tasks_clone.remove(&task_id);
                    return;
                }
            };

            if cancelled_flag.load(Ordering::SeqCst) {
                tracing::warn!("⚠️  任务 #{} 在获取 permit 期间被取消，停止启动", task_id);
                tasks_clone.remove(&task_id);
                return;
            }

            if let Err(e) = f.await {
                tracing::error!("Task {} failed: {:?}", task_id, e);
                if let Some(mut task) = tasks_clone.get_mut(&task_id) {
                    task.state = TaskState::Failed;
                }
            } else if let Some(mut task) = tasks_clone.get_mut(&task_id) {
                task.state = TaskState::Success;
            }

            // 任务执行结束后从活跃任务表中移除，避免内存泄漏
            tasks_clone.remove(&task_id);
        });
        
        // 更新任务的 handle
        if let Some(mut task) = self.tasks.get_mut(&task_id) {
            task.handle = Some(handle);
        }
        
        Ok(())
    }
    
    /// 创建并预注册任务的取消标志（在 start_task 之前调用）
    pub fn create_cancelled_flag(&self, task_id: i64) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        // 预注册任务（handle 为 None，稍后会在 start_task 中设置）
        self.tasks.insert(task_id, TaskHandle {
            state: TaskState::StartBuild,
            handle: None,
            cancelled: cancelled.clone(),
        });
        cancelled
    }
    
    /// 获取任务的取消标志
    #[allow(dead_code)]
    pub fn get_cancelled_flag(&self, task_id: i64) -> Option<Arc<AtomicBool>> {
        self.tasks.get(&task_id).map(|task| task.cancelled.clone())
    }
    
    #[allow(dead_code)]
    pub fn get_task_state(&self, task_id: i64) -> Option<TaskState> {
        self.tasks.get(&task_id).map(|r| r.state)
    }
    
    #[allow(dead_code)]
    pub fn update_task_state(&self, task_id: i64, state: TaskState) {
        if let Some(mut task) = self.tasks.get_mut(&task_id) {
            task.state = state;
        }
    }
    
    pub async fn cancel_task(
        &self, 
        task_id: i64,
    ) -> anyhow::Result<()> {
        tracing::info!("取消任务 #{}", task_id);
        
        // 设置取消标志（不立即移除任务，让取消标志能够被检查）
        if let Some(task) = self.tasks.get(&task_id) {
            task.cancelled.store(true, Ordering::SeqCst);
        } else {
            tracing::warn!("任务 #{} 不在 TaskManager 中", task_id);
            return Err(anyhow::anyhow!("Task {} not found in TaskManager", task_id));
        }
        
        // 等待一小段时间，让子进程清理逻辑检测取消标志并终止进程组
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        
        // 终止异步任务
        if let Some((_, task)) = self.tasks.remove(&task_id) {
            if let Some(handle) = task.handle {
                handle.abort();
            }
        }
        
        Ok(())
    }
    
    #[allow(dead_code)]
    pub fn is_processing(&self) -> bool {
        !self.tasks.is_empty()
    }
    
    #[allow(dead_code)]
    pub fn has_task(&self, task_id: i64) -> bool {
        self.tasks.contains_key(&task_id)
    }
}

