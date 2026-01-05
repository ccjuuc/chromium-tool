use std::sync::Arc;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    Json,
};
use crate::api::AppState;
use crate::model::build::BuildRequest;

pub async fn build_page(State(state): State<AppState>) -> impl IntoResponse {
    if state.db_pool.is_none() {
        let db_server = &state.config.server.db_server;
        Html(format!("go to <a href='http://{}'/>home</a>", db_server))
    } else {
        let html_content = include_str!("../../templates/pkgbuild.html");
        Html(html_content.to_string())
    }
}

pub async fn build_package(
    State(state): State<AppState>,
    Json(request): Json<BuildRequest>,
) -> impl IntoResponse {
    // 基本验证
    if request.branch.is_empty() || request.platform.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "Branch and platform are required",
        ).into_response();
    }
    
    // 检查服务是否可用（从 AppState 中获取，避免每次请求都创建新实例）
    let _task_service = match &state.task_service {
        Some(service) => service,
        None => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Database not available",
            ).into_response();
        }
    };
    
    let build_service = match &state.build_service {
        Some(service) => service,
        None => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Build service not available",
            ).into_response();
        }
    };
    
    let task_repo = match &state.task_repo {
        Some(repo) => repo,
        None => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Database not available",
            ).into_response();
        }
    };
    
    // 验证架构列表
    if request.architectures.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "At least one architecture is required",
        ).into_response();
    }
    
    // 获取服务器锁，防止同一服务器并发创建任务（防止重入问题）
    let server_lock = state.get_server_lock(&request.server);
    let _guard = server_lock.lock().await;
    
    // 检查同一服务器是否有正在执行的任务（不包括 pending，因为 pending 会排队）
    let has_running = match task_repo.has_running_task_on_server(&request.server).await {
        Ok(true) => {
            // 获取排队任务数量（包括 pending）
            let pending_count = task_repo.get_running_task_count_on_server(&request.server).await.unwrap_or(0);
            tracing::info!("⚠️  服务器 {} 已有任务正在执行，新任务将排队等待（当前排队: {} 个）", request.server, pending_count);
            true
        }
        Ok(false) => false,
        Err(e) => {
            tracing::warn!("⚠️  检查服务器任务状态失败: {}", e);
            false
        }
    };
    
    let server_name = request.server.clone();
    let mut response_task_ids = Vec::new();
    let mut errors = Vec::new();
    
    // 统一逻辑：先创建 Pending 状态的任务
    if request.architectures.len() == 1 {
        // 单架构任务
        match build_service.create_build_task(request.clone(), task_repo.as_ref()).await {
            Ok(task_id) => response_task_ids.push(task_id),
            Err(e) => return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create build task: {}", e),
            ).into_response(),
        }
    } else {
        // 多架构任务：创建父任务
        let parent_task = crate::model::task::CreateTask {
            branch: request.branch.clone(),
            oem_name: String::new(),  // 已删除 OEM 配置
            commit_id: request.commit_id.clone().unwrap_or_default(),
            pkg_flag: format!("{} [{}]", request.pkg_flag, request.architectures.join(", ")),
            is_increment: request.is_increment,
            is_signed: request.is_signed,
            server: request.server.clone(),
            parent_id: None,
            architecture: None,
        };
        
        match task_repo.create(&parent_task).await {
            Ok(parent_id) => {
                response_task_ids.push(parent_id); // 记录父任务ID
                
                // 创建子任务（全部 Pending）
                for arch in &request.architectures {
                    let mut sub_request = request.clone();
                    sub_request.architectures = vec![arch.clone()];
                    sub_request.is_x64 = arch == "x64" || arch == "x86";
                    
                    match build_service.create_child_task(
                        sub_request,
                        parent_id,
                        task_repo.as_ref(),
                    ).await {
                        Ok(_) => {}, // 子任务ID不一定要返回给前端，或者可以附加
                        Err(e) => errors.push(format!("Failed to create child task for {}: {}", arch, e)),
                    }
                }
            },
            Err(e) => return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create parent task: {}", e),
            ).into_response(),
        }
    }
    
    if !errors.is_empty() {
         return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Errors creating tasks: {}", errors.join("; ")),
        ).into_response();
    }

    // 后续处理：根据 has_running 决定是否触发队列消费者
    if has_running {
        // 有任务运行，新任务已入队（Pending），直接返回排队信息
        let queue_count = task_repo.get_running_task_count_on_server(&server_name).await.unwrap_or(0);
        let msg = if request.architectures.len() == 1 {
            format!("Build task created (task_id: {}), queued and waiting (queue position: {})", response_task_ids[0], queue_count)
        } else {
            format!("Parent task {} created with children, queued and waiting (queue position: {})", response_task_ids[0], queue_count)
        };
        (axum::http::StatusCode::OK, msg).into_response()
    } else {
        // 无任务运行，触发队列消费者启动下一个 Pending 任务（即刚才创建的第一个，或者更早的）
        let app_state = Arc::new(state.clone());
        // start_next_pending_task 是异步的，我们等待它启动（或者 spawn 也可以，但 await 更稳妥确保触发）
        app_state.start_next_pending_task(server_name).await;
        
        let msg = if request.architectures.len() == 1 {
            format!("Build task started, task_id: {}", response_task_ids[0])
        } else {
            format!("Parent task {} created, build sequence started", response_task_ids[0])
        };
        (axum::http::StatusCode::OK, msg).into_response()
    }
}

