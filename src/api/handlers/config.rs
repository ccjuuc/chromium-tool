use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use crate::api::AppState;
use crate::util::git;
use std::path::PathBuf;
use serde::Serialize;

#[derive(Serialize)]
pub struct BranchListResponse {
    pub branches: Vec<String>,
    pub default_branch: Option<String>,
}

pub async fn server_list(State(state): State<AppState>) -> impl IntoResponse {
    serde_json::to_string(&state.config.server)
        .map(|json| (StatusCode::OK, json))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to serialize"))
}

#[allow(dead_code)]
pub async fn oem_list(State(state): State<AppState>) -> impl IntoResponse {
    serde_json::to_string(&state.config.oem)
        .map(|json| (StatusCode::OK, json))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to serialize"))
}

pub async fn custom_args_list(State(state): State<AppState>) -> impl IntoResponse {
    serde_json::to_string(&state.config.custom_args)
        .map(|json| (StatusCode::OK, json))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to serialize"))
}

pub async fn build_args_list(State(state): State<AppState>) -> impl IntoResponse {
    serde_json::to_string(&state.config.build_args)
        .map(|json| (StatusCode::OK, json))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to serialize"))
}

/// 获取分支列表
pub async fn branch_list(State(state): State<AppState>) -> impl IntoResponse {
    let src_path = match state.config.get_src_path() {
        Ok(path) => PathBuf::from(path),
        Err(e) => {
            tracing::error!("获取源码路径失败: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BranchListResponse {
                    branches: Vec::new(),
                    default_branch: None,
                }),
            ).into_response();
        }
    };
    
    match git::get_branch_list(&src_path).await {
        Ok(branches) => {
            // 确定默认分支：优先 main，其次 master，再次 develop
            let default_branch = if branches.contains(&"main".to_string()) {
                Some("main".to_string())
            } else if branches.contains(&"master".to_string()) {
                Some("master".to_string())
            } else if branches.contains(&"develop".to_string()) {
                Some("develop".to_string())
            } else {
                branches.first().cloned()
            };
            
            (
                StatusCode::OK,
                Json(BranchListResponse {
                    branches,
                    default_branch,
                }),
            ).into_response()
        }
        Err(e) => {
            tracing::error!("获取分支列表失败: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(BranchListResponse {
                    branches: Vec::new(),
                    default_branch: None,
                }),
            ).into_response()
        }
    }
}

