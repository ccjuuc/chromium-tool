use axum::{Router, routing::get, routing::post, middleware, extract::Request, response::{IntoResponse, Response}};
use axum::http::{HeaderMap, StatusCode};
use tower::limit::ConcurrencyLimitLayer;
use crate::api::{AppState};
use crate::api::handlers;
use crate::api::ws;

pub fn create_router(state: AppState) -> Router {
    Router::new()
        // OEM 路由（根路径默认进入 OEM 页面）
        .route("/", get(handlers::oem::oem_page))
        .route("/oem", get(handlers::oem::oem_page))
        .route(
            "/oem/icon_preview",
            get(handlers::oem::icon_batch_preview_page),
        )
        .route("/icon_preview_list", get(handlers::oem::list_icon_preview_files))
        .route(
            "/icon_preview_select_directory",
            post(handlers::oem::select_icon_preview_directory),
        )
        .route(
            "/icon_preview_batch_convert",
            post(handlers::oem::icon_preview_batch_convert),
        )
        .route("/convert_image", post(handlers::oem::convert_image))
        .route("/convert_output/:file_name", get(handlers::oem::get_convert_output))
        .route(
            "/convert_output_svg/:file_name",
            get(handlers::oem::get_convert_output_svg),
        )
        .route("/oem_convert", post(handlers::oem::oem_convert))
        .route("/add_rounded_corners", post(handlers::oem::add_rounded_corners))
        
        // 构建路由（仅对 /build_package POST 请求限制并发为 1，避免阻塞页面与 OEM 路由）
        .route("/build", get(handlers::build::build_page))
        .route(
            "/build_package",
            post(handlers::build::build_package).layer(ConcurrencyLimitLayer::new(1)),
        )
        
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
        
        .layer(middleware::from_fn(require_same_origin))
        .with_state(state)
}

fn request_origin_allowed(headers: &HeaderMap, allowed_hosts: &[String]) -> bool {
    let Some(host) = headers.get("host").and_then(|value| value.to_str().ok()) else {
        return false;
    };
    if !allowed_hosts.iter().any(|allowed| allowed.eq_ignore_ascii_case(host)) {
        return false;
    }
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok())
        .is_some_and(|site| !matches!(site, "same-origin" | "none")) {
        return false;
    }
    if let Some(origin) = headers.get("origin") {
        let Ok(origin) = origin.to_str() else { return false; };
        let Ok(uri) = origin.parse::<axum::http::Uri>() else { return false; };
        if uri.scheme_str() != Some("http")
            || !uri.authority().is_some_and(|authority| authority.as_str().eq_ignore_ascii_case(host))
            || uri.path_and_query().is_some_and(|path| path.as_str() != "/") {
            return false;
        }
    }
    true
}

async fn require_same_origin(request: Request, next: middleware::Next) -> Response {
    let bind = std::env::var("CHROMIUM_TOOL_BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let Ok(bind) = bind.parse::<std::net::SocketAddr>() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Invalid server bind address").into_response();
    };
    let mut hosts = vec![format!("localhost:{}", bind.port()), format!("127.0.0.1:{}", bind.port()), format!("[::1]:{}", bind.port())];
    if !bind.ip().is_unspecified() {
        hosts.push(bind.to_string());
    }
    // Explicit public bindings must also declare the hostnames clients will use.
    if let Ok(extra) = std::env::var("CHROMIUM_TOOL_ALLOWED_HOSTS") {
        hosts.extend(extra.split(',').map(str::trim).filter(|host| !host.is_empty()).map(str::to_string));
    }
    if !request_origin_allowed(request.headers(), &hosts) {
        return (StatusCode::FORBIDDEN, "This endpoint requires the configured host and a same-origin request").into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    #[test]
    fn local_requests_reject_cross_origin_and_rebound_hosts() {
        let allowed = vec!["127.0.0.1:3000".to_string()];
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:3000".parse().unwrap());
        assert!(request_origin_allowed(&headers, &allowed));
        headers.insert("origin", "http://127.0.0.1:3000".parse().unwrap());
        assert!(request_origin_allowed(&headers, &allowed));
        headers.insert("origin", "https://127.0.0.1:3000".parse().unwrap());
        assert!(!request_origin_allowed(&headers, &allowed));
        headers.insert("origin", "null".parse().unwrap());
        assert!(!request_origin_allowed(&headers, &allowed));
        headers.insert("origin", "https://example.test".parse().unwrap());
        assert!(!request_origin_allowed(&headers, &allowed));
        headers.remove("origin");
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!request_origin_allowed(&headers, &allowed));
        headers.remove("sec-fetch-site");
        headers.insert("host", "rebound.example:3000".parse().unwrap());
        assert!(!request_origin_allowed(&headers, &allowed));
    }
}
