use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};
use crate::api::AppState;

pub async fn server_list(State(state): State<AppState>) -> impl IntoResponse {
    serde_json::to_string(&state.config.server)
        .map(|json| (StatusCode::OK, json))
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Failed to serialize"))
}

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

