use axum::{Router, routing::get, routing::post};
use axum::http::Method;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{CorsLayer, Any};
use crate::api::{AppState};
use crate::api::handlers;
use crate::api::ws;

pub fn create_router(state: AppState) -> Router {
    // 配置 CORS，确保正确处理预检请求
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any)
        .allow_credentials(false)
        .expose_headers(Any)
        .max_age(std::time::Duration::from_secs(3600));
    
    Router::new()
        // OEM 路由
        .route("/oem", get(handlers::oem::oem_page))
        .route("/convert_image", post(handlers::oem::convert_image))
        .route("/oem_convert", post(handlers::oem::oem_convert))
        .route("/add_rounded_corners", post(handlers::oem::add_rounded_corners))
        
        // 构建路由（限制并发为 1）
        .route("/", get(handlers::build::build_page))
        .route("/build_package", post(handlers::build::build_package))
        .layer(ConcurrencyLimitLayer::new(1))  // 高优先级：限流
        
        // 任务路由
        .route("/task_list", get(handlers::task::task_list))
        .route("/add_task", post(handlers::task::add_task))
        .route("/update_task", post(handlers::task::update_task))
        .route("/delete_task", post(handlers::task::delete_task))
        .route("/download/*file_path", get(handlers::task::download_installer))
        .route("/task_log/:task_id", get(handlers::task::get_task_log))
        
        // WebSocket 路由
        .route("/ws/task_log/:task_id", axum::routing::get(ws::ws_handler))
        
        // 配置路由
        .route("/server_list", get(handlers::config::server_list))
        .route("/branch_list", get(handlers::config::branch_list))
        .route("/custom_args_list", get(handlers::config::custom_args_list))
        .route("/build_args_list", get(handlers::config::build_args_list))
        
        // ID 查找路由
        .route("/generate_id", post(handlers::id_finder::generate_id))
        .route("/search_id", post(handlers::id_finder::search_id))
        
        // CORS 层应该在最后，确保所有路由都应用
        .layer(cors)
        .with_state(state)
}

