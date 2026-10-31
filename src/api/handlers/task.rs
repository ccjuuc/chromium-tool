use axum::{
    extract::{State, Path as AxumPath, Json},
    http::{StatusCode, header},
    response::{Response, IntoResponse},
};
use axum::Json as AxumJson;
use crate::api::AppState;
use crate::model::task::{CreateTask, UpdateTask, DeleteTask};
use crate::repository::task::TaskRepository;
use std::path::Path;

pub async fn task_list(State(state): State<AppState>) -> impl IntoResponse {
    let task_service = match &state.task_service {
        Some(service) => service,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                AxumJson(serde_json::json!({"error": "Database not available"})),
            ).into_response();
        }
    };
    
    match task_service.list_tasks().await {
        Ok(tasks) => {
            let json_result = serde_json::json!({"tasks": tasks});
            (StatusCode::OK, AxumJson(json_result)).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                AxumJson(serde_json::json!({"error": format!("Failed to fetch tasks: {}", e)})),
            ).into_response()
        }
    }
}

pub async fn add_task(
    State(state): State<AppState>,
    Json(payload): Json<CreateTask>,
) -> impl IntoResponse {
    let task_repo = match &state.db_pool {
        Some(pool) => TaskRepository::new(pool.clone()),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database not available",
            ).into_response();
        }
    };
    
    match task_repo.create(&payload).await {
        Ok(task_id) => (StatusCode::OK, task_id.to_string()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create task: {}", e),
        ).into_response(),
    }
}

pub async fn update_task(
    State(state): State<AppState>,
    Json(payload): Json<UpdateTask>,
) -> impl IntoResponse {
    let task_repo = match &state.db_pool {
        Some(pool) => TaskRepository::new(pool.clone()),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database not available",
            ).into_response();
        }
    };
    
    // 更新状态
    if let Some(state) = payload.state {
        if let Err(e) = task_repo.update_state(payload.id, state, payload.commit_id.as_deref()).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to update task state: {}", e),
            ).into_response();
        }
    }
    
    // 更新完成信息
    if payload.end_time.is_some() || payload.storage_path.is_some() || payload.installer.is_some() {
        let end_time = payload.end_time.as_deref().unwrap_or("");
        let storage_path = payload.storage_path.as_deref().unwrap_or("");
        let installer = payload.installer.as_deref().unwrap_or("");
        
        if let Err(e) = task_repo.update_completion(
            payload.id,
            end_time,
            storage_path,
            installer,
            payload.commit_id.as_deref(),
        ).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to update task completion: {}", e),
            ).into_response();
        }
    }
    
    if let Some(service) = &state.task_service {
        service.cache().invalidate(payload.id).await;
    }

    (StatusCode::OK, "Task updated").into_response()
}

pub async fn delete_task(
    State(state): State<AppState>,
    Json(payload): Json<DeleteTask>,
) -> impl IntoResponse {
    let task_service = match &state.task_service {
        Some(service) => service,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database not available",
            ).into_response();
        }
    };
    
    let task_repo = match &state.task_repo {
        Some(repo) => repo.clone(),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database not available",
            ).into_response();
        }
    };
    
    let task_id = payload.task_id;
    
    // 获取任务信息，检查是否是父任务
    let task = match task_repo.find_by_id(task_id).await {
        Ok(task) => task,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                format!("Task not found: {}", e),
            ).into_response();
        }
    };
    
    // 判断是否是父任务（parent_id 为 None 且有子任务）还是单任务（parent_id 为 None 但没有子任务）
    if task.parent_id.is_none() {
        // 查找所有子任务（通过查询数据库）
        // 使用 TaskRepository 的 list 方法，然后过滤出子任务
        let all_tasks = match task_repo.list().await {
            Ok(tasks) => tasks,
            Err(e) => {
                tracing::warn!("Failed to fetch tasks: {}", e);
                Vec::new()
            }
        };
        
        // 过滤出当前任务的子任务
        let child_tasks: Vec<_> = all_tasks.into_iter()
            .filter(|t| t.parent_id == Some(task_id))
            .collect();
        
        if !child_tasks.is_empty() {
            // 这是父任务，有子任务，需要取消所有子任务
            tracing::info!("🛑 父任务 #{} 有 {} 个子任务，开始取消所有子任务...", task_id, child_tasks.len());
            
            for child_task in child_tasks {
                // 检查任务状态是否为非终态（正在运行）
                let is_running = !matches!(child_task.state, crate::model::state::TaskState::Success | crate::model::state::TaskState::Failed | crate::model::state::TaskState::Cancelled);
                
                if is_running {
                    // 尝试从 TaskManager 取消任务
                    let _ = task_service.manager().cancel_task(child_task.id).await;
                    
                    // 更新数据库状态为 cancelled
                    if let Err(e) = task_repo.update_state(child_task.id, crate::model::state::TaskState::Cancelled, None).await {
                        tracing::warn!("Failed to update child task {} state: {}", child_task.id, e);
                    }
                }
                task_service.cache().invalidate(child_task.id).await;
                state.ws_manager.remove_channel(child_task.id);
            }
            
            // 父任务本身不会执行，所以只需要更新数据库状态
            if let Err(e) = task_repo.update_state(task_id, crate::model::state::TaskState::Cancelled, None).await {
                tracing::warn!("Failed to update parent task {} state: {}", task_id, e);
            }
        } else {
            // 这是单任务（parent_id 为 None 但没有子任务），需要取消自己
            let is_running = !matches!(task.state, crate::model::state::TaskState::Success | crate::model::state::TaskState::Failed | crate::model::state::TaskState::Cancelled);
            
            if is_running {
                // 尝试从 TaskManager 取消任务
                if let Err(e) = task_service.manager().cancel_task(task_id).await {
                    tracing::warn!("Task {} not in TaskManager: {}", task_id, e);
                }
                
                // 更新数据库状态为 cancelled
                if let Err(e) = task_repo.update_state(task_id, crate::model::state::TaskState::Cancelled, None).await {
                    tracing::warn!("Failed to update task {} state: {}", task_id, e);
                }
            }
        }
    } else {
        // 如果是子任务，尝试取消
        let is_running = !matches!(task.state, crate::model::state::TaskState::Success | crate::model::state::TaskState::Failed | crate::model::state::TaskState::Cancelled);
        
        if is_running {
            // 尝试从 TaskManager 取消任务
            if let Err(e) = task_service.manager().cancel_task(task_id).await {
                tracing::warn!("Task {} not in TaskManager: {}", task_id, e);
            }
            
            // 更新数据库状态为 cancelled
            if let Err(e) = task_repo.update_state(task_id, crate::model::state::TaskState::Cancelled, None).await {
                tracing::warn!("Failed to update task {} state: {}", task_id, e);
            }
        }
    }
    
    // 删除数据库记录（包括所有子任务）
    if let Err(e) = task_repo.delete(task_id).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to delete task: {}", e)).into_response();
    }
    task_service.cache().invalidate(task_id).await;
    state.ws_manager.remove_channel(task_id);
    
    (StatusCode::OK, "Task deleted").into_response()
}

pub async fn download_installer(
    State(state): State<AppState>,
    AxumPath(file_path): AxumPath<String>,
) -> impl IntoResponse {
    let backup_path = match state.config.get_backup_path() {
        Ok(path) if !path.trim().is_empty() => path,
        Ok(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Backup path is empty in configuration".to_string(),
            ).into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Invalid backup path configuration: {}", e),
            ).into_response();
        }
    };
    
    let base_dir = Path::new(backup_path);
    let candidate_file = base_dir.join(file_path.trim_start_matches('/'));
    
    if !candidate_file.exists() {
        return (StatusCode::NOT_FOUND, "File not found").into_response();
    }

    // 校验规范化路径仍在 backup_path 目录内，防止 ../ 路径穿越攻击
    let download_file = match crate::util::path::validate_path(&candidate_file, base_dir) {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::FORBIDDEN, "Access denied: invalid file path").into_response();
        }
    };

    if !download_file.is_file() {
        return (StatusCode::BAD_REQUEST, "Requested path is not a file").into_response();
    }
    
    let file_name = match download_file
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
    {
        Some(name) => name,
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Invalid file name").into_response();
        }
    };
    
    let file = match tokio::fs::read(&download_file).await {
        Ok(content) => content,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to read file: {}", e),
            ).into_response();
        }
    };
    
    match Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", file_name),
        )
        .body(axum::body::Body::from(file))
    {
        Ok(response) => response,
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to build response: {}", e),
        )
            .into_response(),
    }
}

pub async fn get_task_log(
    State(state): State<AppState>,
    AxumPath(task_id): AxumPath<i64>,
) -> impl IntoResponse {
    let task_repo = match &state.db_pool {
        Some(pool) => TaskRepository::new(pool.clone()),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                AxumJson(serde_json::json!({"error": "Database not available"})),
            ).into_response();
        }
    };
    
    match task_repo.get_build_log(task_id).await {
        Ok(Some(log)) => {
            (StatusCode::OK, AxumJson(serde_json::json!({"log": log}))).into_response()
        }
        Ok(None) => {
            (StatusCode::OK, AxumJson(serde_json::json!({"log": ""}))).into_response()
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                AxumJson(serde_json::json!({"error": format!("Failed to get task log: {}", e)})),
            ).into_response()
        }
    }
}

