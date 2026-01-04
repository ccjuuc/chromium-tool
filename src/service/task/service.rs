use crate::service::task::{TaskManager, TaskCache};
use crate::repository::task::TaskRepository;
use crate::model::task::Task;

pub struct TaskService {
    manager: TaskManager,
    cache: TaskCache,
    repo: TaskRepository,
}

impl std::fmt::Debug for TaskService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskService")
            .field("manager", &"TaskManager")
            .field("cache", &"TaskCache")
            .field("repo", &"TaskRepository")
            .finish()
    }
}

impl TaskService {
    pub fn new(repo: TaskRepository) -> Self {
        Self {
            manager: TaskManager::new(1),  // 最多 1 个并发任务
            cache: TaskCache::new(),
            repo,
        }
    }
    
    #[allow(dead_code)]
    pub async fn get_task(&self, id: i64) -> anyhow::Result<Task> {
        // 先查缓存
        if let Some(task) = self.cache.get(id).await {
            return Ok(task);
        }
        
        // 查数据库
        let task = self.repo.find_by_id(id).await?;
        
        // 更新缓存
        self.cache.insert(id, task.clone()).await;
        
        Ok(task)
    }
    
    pub async fn list_tasks(&self) -> anyhow::Result<Vec<Task>> {
        let tasks = self.repo.list().await?;
        
        // 更新缓存
        for task in &tasks {
            self.cache.insert(task.id, task.clone()).await;
        }
        
        Ok(tasks)
    }
    
    pub fn manager(&self) -> &TaskManager {
        &self.manager
    }
}
