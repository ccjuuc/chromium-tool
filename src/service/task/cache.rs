use moka::future::Cache;
use std::time::Duration;
use crate::model::task::Task;

pub struct TaskCache {
    cache: Cache<i64, Task>,
}

impl TaskCache {
    pub fn new() -> Self {
        let cache = Cache::builder()
            .max_capacity(1000)
            .time_to_live(Duration::from_secs(300))
            .time_to_idle(Duration::from_secs(60))
            .build();
        
        Self { cache }
    }
    
    #[allow(dead_code)]
    pub async fn get(&self, id: i64) -> Option<Task> {
        self.cache.get(&id).await
    }
    
    pub async fn insert(&self, id: i64, task: Task) {
        self.cache.insert(id, task).await;
    }
    
    #[allow(dead_code)]
    pub async fn invalidate(&self, id: i64) {
        self.cache.invalidate(&id).await;
    }
    
    #[allow(dead_code)]
    pub async fn invalidate_all(&self) {
        self.cache.invalidate_all();
    }
}

