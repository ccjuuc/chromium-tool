use sqlx::SqlitePool;
use crate::error::{AppError, AppResult};
use crate::model::task::{Task, CreateTask};
use crate::model::state::TaskState;

const TASKLIST_QUERY: &str = r#"
  SELECT id, start_time, branch_name, end_time, oem_name, commit_id, pkg_flag, is_signed, is_increment, storage_path, installer, state, server, parent_id, architecture, build_log
  FROM pkg
  ORDER BY COALESCE(parent_id, id) DESC, id ASC
"#;

const ADD_TASK: &str = r#"
INSERT INTO pkg (start_time, branch_name, oem_name, commit_id, pkg_flag, is_increment, is_signed, server, parent_id, architecture)
VALUES (datetime('now', 'localtime'), ?, ?, ?, ?, ?, ?, ?, ?, ?)
RETURNING id
"#;

const UPDATE_TASK: &str = r#"
UPDATE pkg
SET end_time = ?,
    storage_path = ?,
    installer = ?,
    state = ?
WHERE id = ?
"#;

const UPDATE_TASK_COMMIT_ID: &str = r#"
UPDATE pkg
SET end_time = ?,
    storage_path = ?,
    installer = ?,
    state = ?,
    commit_id = ?
WHERE id = ?
"#;

#[derive(Clone)]
pub struct TaskRepository {
    pool: SqlitePool,
}

impl TaskRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    
    pub async fn create(&self, task: &CreateTask) -> AppResult<i64> {
        let task_id = sqlx::query_scalar(ADD_TASK)
            .bind(&task.branch)
            .bind(&task.oem_name)
            .bind(&task.commit_id)
            .bind(&task.pkg_flag)
            .bind(task.is_increment)
            .bind(task.is_signed)
            .bind(&task.server)
            .bind(task.parent_id)
            .bind(&task.architecture)
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(task_id)
    }
    
    #[allow(dead_code)]
    pub async fn find_by_id(&self, id: i64) -> AppResult<Task> {
        let row = sqlx::query("SELECT * FROM pkg WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(self.row_to_task(&row))
    }
    
    pub async fn list(&self) -> AppResult<Vec<Task>> {
        let rows = sqlx::query(TASKLIST_QUERY)
            .fetch_all(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        let tasks: Vec<Task> = rows.iter()
            .map(|row| self.row_to_task(row))
            .collect();
        
        Ok(tasks)
    }
    
    /// 检查同一服务器是否有正在执行的任务（不包括 pending 状态）
    /// 只检查正在执行的任务，pending 任务不算，因为它们会排队等待
    pub async fn has_running_task_on_server(&self, server: &str) -> AppResult<bool> {
        // 查询同一服务器上正在执行的任务（排除 pending、success、failed 状态）
        // pending 任务不算，因为它们会排队等待
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pkg 
             WHERE server = ? 
             AND state NOT IN ('pending', 'success', 'failed')"
        )
            .bind(server)
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(count > 0)
    }
    
    /// 获取同一服务器上正在执行的任务数量（用于排队提示）
    /// 不包括 pending 状态的任务
    pub async fn get_running_task_count_on_server(&self, server: &str) -> AppResult<i64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pkg 
             WHERE server = ? 
             AND state NOT IN ('pending', 'success', 'failed')"
        )
            .bind(server)
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(count)
    }
    
    /// 获取同一服务器上最早创建的 pending 任务（用于排队启动）
    pub async fn get_next_pending_task_on_server(&self, server: &str) -> AppResult<Option<i64>> {
        let task_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM pkg 
             WHERE server = ? 
             AND state = 'pending'
             AND parent_id IS NULL
             ORDER BY id ASC
             LIMIT 1"
        )
            .bind(server)
            .fetch_optional(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(task_id)
    }
    
    /// 获取下一个 pending 子任务（用于启动构建）
    pub async fn get_next_pending_child_task_on_server(&self, server: &str) -> AppResult<Option<i64>> {
        let task_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM pkg 
             WHERE server = ? 
             AND state = 'pending'
             AND parent_id IS NOT NULL
             ORDER BY parent_id ASC, id ASC
             LIMIT 1"
        )
            .bind(server)
            .fetch_optional(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(task_id)
    }
    
    
    /// 更新父子任务的 commit_id（在第一次获取 commit_id 时调用）
    pub async fn update_family_commit_id(&self, task_id: i64, commit_id: &str) -> AppResult<()> {
        // 获取当前任务信息
        let task = self.find_by_id(task_id).await?;
        
        if let Some(parent_id) = task.parent_id {
            // 当前任务是子任务，更新父任务和所有兄弟子任务的 commit_id
            sqlx::query("UPDATE pkg SET commit_id = ? WHERE id = ?")
                .bind(commit_id)
                .bind(parent_id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
            
            sqlx::query("UPDATE pkg SET commit_id = ? WHERE parent_id = ?")
                .bind(commit_id)
                .bind(parent_id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
        } else {
            // 当前任务是父任务，更新所有子任务的 commit_id
            sqlx::query("UPDATE pkg SET commit_id = ? WHERE parent_id = ?")
                .bind(commit_id)
                .bind(task_id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
            
            // 更新父任务自己的 commit_id
            sqlx::query("UPDATE pkg SET commit_id = ? WHERE id = ?")
                .bind(commit_id)
                .bind(task_id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
        }
        
        Ok(())
    }
    
    pub async fn update_state(
        &self,
        id: i64,
        state: TaskState,
        commit_id: Option<&str>,
    ) -> AppResult<()> {
        let state_str = state.as_str();
        
        if let Some(commit_id) = commit_id {
            sqlx::query(UPDATE_TASK_COMMIT_ID)
                .bind("")
                .bind("")
                .bind("")
                .bind(state_str)
                .bind(commit_id)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
        } else {
            sqlx::query(UPDATE_TASK)
                .bind("")
                .bind("")
                .bind("")
                .bind(state_str)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
        }
        
        Ok(())
    }
    
    pub async fn update_completion(
        &self,
        id: i64,
        end_time: &str,
        storage_path: &str,
        installer: &str,
        commit_id: Option<&str>,
    ) -> AppResult<()> {
        let state_str = TaskState::Success.as_str();
        
        if let Some(commit_id) = commit_id {
            sqlx::query(UPDATE_TASK_COMMIT_ID)
                .bind(end_time)
                .bind(storage_path)
                .bind(installer)
                .bind(state_str)
                .bind(commit_id)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
        } else {
            sqlx::query(UPDATE_TASK)
                .bind(end_time)
                .bind(storage_path)
                .bind(installer)
                .bind(state_str)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(AppError::Database)?;
        }
        
        Ok(())
    }
    
    pub async fn delete(&self, id: i64) -> AppResult<()> {
        // 先删除所有子任务（级联删除）
        sqlx::query("DELETE FROM pkg WHERE parent_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        // 然后删除父任务本身
        sqlx::query("DELETE FROM pkg WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(())
    }
    
    fn row_to_task(&self, row: &sqlx::sqlite::SqliteRow) -> Task {
        use sqlx::Row;
        
        let state_str: String = row.get("state");
        let state = state_str.parse().unwrap_or(TaskState::Pending);
        
        Task {
            id: row.get("id"),
            start_time: row.get("start_time"),
            end_time: row.try_get("end_time").ok(),
            branch_name: row.get("branch_name"),
            oem_name: row.get("oem_name"),
            commit_id: row.get("commit_id"),
            pkg_flag: row.get("pkg_flag"),
            is_signed: row.get("is_signed"),
            is_increment: row.get("is_increment"),
            storage_path: row.get("storage_path"),
            installer: row.get("installer"),
            state,
            server: row.get("server"),
            parent_id: {
                // 正确处理 NULL 值：如果 parent_id 是 NULL，try_get 会返回错误，ok() 会转换为 None
                // 如果值是 0，也将其视为 None（因为 0 不应该作为有效的 parent_id）
                match row.try_get::<Option<i64>, _>("parent_id") {
                    Ok(Some(0)) => None,  // 0 不应该作为有效的 parent_id，将其视为 None
                    Ok(val) => val,
                    Err(_) => None,  // NULL 值或字段不存在
                }
            },
            architecture: row.try_get("architecture").ok(),
            build_log: row.try_get("build_log").ok(),
        }
    }
    
    /// 追加构建日志
    pub async fn append_build_log(&self, task_id: i64, log_line: &str) -> AppResult<()> {
        // 获取当前日志
        let current_log: Option<Option<String>> = sqlx::query_scalar("SELECT build_log FROM pkg WHERE id = ?")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        // 追加新日志（限制最大长度，避免数据库过大）
        let max_log_size = 100_000; // 100KB
        let new_log = if let Some(Some(log)) = current_log {
            let mut updated = log + "\n" + log_line;
            // 如果日志太长，只保留最后的部分
            if updated.len() > max_log_size {
                updated = updated.chars().rev().take(max_log_size).collect::<String>().chars().rev().collect();
            }
            updated
        } else {
            log_line.to_string()
        };
        
        sqlx::query("UPDATE pkg SET build_log = ? WHERE id = ?")
            .bind(&new_log)
            .bind(task_id)
            .execute(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(())
    }
    
    /// 获取构建日志
    pub async fn get_build_log(&self, task_id: i64) -> AppResult<Option<String>> {
        let log: Option<Option<String>> = sqlx::query_scalar("SELECT build_log FROM pkg WHERE id = ?")
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(log.flatten())
    }

    /// 重置所有正在执行的任务状态为 failed（用于服务器重启时清理旧任务）
    pub async fn reset_running_tasks(pool: &SqlitePool) -> AppResult<u64> {
        let result = sqlx::query(
            "UPDATE pkg 
             SET state = 'failed', end_time = datetime('now', 'localtime') 
             WHERE state NOT IN ('pending', 'success', 'failed')"
        )
            .execute(pool)
            .await
            .map_err(AppError::Database)?;
        
        Ok(result.rows_affected())
    }
    
    /// 获取父任务的所有子任务
    pub async fn get_child_tasks(&self, parent_id: i64) -> AppResult<Vec<Task>> {
        let rows = sqlx::query("SELECT * FROM pkg WHERE parent_id = ? ORDER BY id ASC")
            .bind(parent_id)
            .fetch_all(&self.pool)
            .await
            .map_err(AppError::Database)?;
        
        let tasks: Vec<Task> = rows.iter()
            .map(|row| self.row_to_task(row))
            .collect();
        
        Ok(tasks)
    }
    
    /// 检查所有子任务是否都完成了 build chrome（状态为 success 或 build chrome 之后的状态）
    pub async fn all_children_completed_chrome(&self, parent_id: i64) -> AppResult<bool> {
        let children = self.get_child_tasks(parent_id).await?;
        
        if children.is_empty() {
            return Ok(false);
        }
        
        // 检查所有子任务是否都完成了 build chrome
        // 完成 build chrome 意味着状态是 success 或者状态是 build chrome 之后的任何状态
        let all_completed = children.iter().all(|child| {
            matches!(
                child.state,
                TaskState::BuildingChrome | 
                TaskState::Combining | 
                TaskState::BuildingInstaller | 
                TaskState::Signing | 
                TaskState::BackingUp | 
                TaskState::Success
            ) || child.state == TaskState::BuildingChrome
        });
        
        Ok(all_completed)
    }
}

// 为 TaskState 实现 FromStr
impl std::str::FromStr for TaskState {
    type Err = String;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(TaskState::Pending),
            "checkout..." => Ok(TaskState::CheckingOut),
            "start build" => Ok(TaskState::StartBuild),
            "clean..." => Ok(TaskState::Cleaning),
            "gen project" => Ok(TaskState::GeneratingProject),
            "build pre_build" => Ok(TaskState::BuildingPreBuild),
            "build base" => Ok(TaskState::BuildingBase),
            "build chrome" => Ok(TaskState::BuildingChrome),
            "combining" => Ok(TaskState::Combining),
            "build installer" => Ok(TaskState::BuildingInstaller),
            "sign" => Ok(TaskState::Signing),
            "backup" => Ok(TaskState::BackingUp),
            "success" => Ok(TaskState::Success),
            "failed" => Ok(TaskState::Failed),
            _ => Err(format!("Unknown state: {}", s)),
        }
    }
}
