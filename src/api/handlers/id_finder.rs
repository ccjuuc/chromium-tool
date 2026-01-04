use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
};
use crate::api::AppState;
use crate::model::id_finder::{GenerateIdRequest, GenerateIdResponse, SearchIdRequest, SearchIdResponse};
use crate::service::id_finder::IdFinder;
use tracing::error;

/// 生成 message_id
pub async fn generate_id(Json(payload): Json<GenerateIdRequest>) -> impl IntoResponse {
    let message_id = IdFinder::generate_message_id(&payload.message, payload.meaning.as_deref());
    
    (
        StatusCode::OK,
        axum::Json(GenerateIdResponse { message_id }),
    )
}

/// 搜索 ID
pub async fn search_id(
    State(state): State<AppState>,
    Json(payload): Json<SearchIdRequest>,
) -> impl IntoResponse {
    // 从 config.toml 获取源码路径
    let src_path = match state.config.get_src_path() {
        Ok(path) => path.to_string(),
        Err(e) => {
            error!("获取源码路径失败: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(SearchIdResponse {
                    ids: Vec::new(),
                    messages: Vec::new(),
                    grd_matches: Vec::new(),
                }),
            );
        }
    };
    
    match IdFinder::search_ids(&payload.search_text, &src_path) {
        Ok((ids, messages, grd_matches)) => {
            (
                StatusCode::OK,
                axum::Json(SearchIdResponse {
                    ids,
                    messages,
                    grd_matches,
                }),
            )
        }
        Err(e) => {
            error!("搜索 ID 失败: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(SearchIdResponse {
                    ids: Vec::new(),
                    messages: Vec::new(),
                    grd_matches: Vec::new(),
                }),
            )
        }
    }
}

