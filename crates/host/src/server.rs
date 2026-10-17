//! HTTP API and web client, reachable only on the tailnet and localhost.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use connect_proto::{HostInfo, PROTOCOL_VERSION, SessionRequest, SessionResponse};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::session;
use crate::state::AppState;

/// Origins of the bundled desktop/Android app and the web dev server.
const APP_ORIGINS: &[&str] =
    &["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost", "http://localhost:5173"];

pub fn router(state: Arc<AppState>) -> Router {
    let index = state.web_dir.join("index.html");
    let cors = CorsLayer::new()
        .allow_origin(APP_ORIGINS.iter().map(|o| HeaderValue::from_static(o)).collect::<Vec<_>>())
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    Router::new()
        .route("/api/info", get(info))
        .route("/api/session", post(create_session))
        .fallback_service(ServeDir::new(&state.web_dir).fallback(ServeFile::new(index)))
        .layer(cors)
        .layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

/// Only this tailnet user's devices (and localhost) get in. The Host header
/// check stops DNS-rebinding pages from talking to us through the browser.
async fn gate(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.rsplit_once(':').map_or(h, |(name, port)| if port.chars().all(|c| c.is_ascii_digit()) { name } else { h }))
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    if !host_allowed(&state, &host) {
        return (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response();
    }
    if !state.gate.allows(peer.ip()).await {
        return (StatusCode::FORBIDDEN, "This device isn't allowed to connect.").into_response();
    }
    next.run(req).await
}

fn host_allowed(state: &AppState, host: &str) -> bool {
    let name = state.name.to_ascii_lowercase();
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == state.tailscale_ip.to_string()
        || host == name
        || host.strip_prefix(&name).is_some_and(|rest| rest.starts_with('.') && rest.ends_with(".ts.net"))
}

async fn info(State(state): State<Arc<AppState>>) -> Json<HostInfo> {
    Json(host_info(&state))
}

pub fn host_info(state: &AppState) -> HostInfo {
    HostInfo {
        name: state.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL_VERSION,
        os: "windows".into(),
        displays: crate::capture::displays().unwrap_or_default(),
    }
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, String)> {
    let params = session::Params {
        offer: req.sdp,
        display: req.display,
        max_size: (req.max_width.unwrap_or(2560).clamp(320, 7680), req.max_height.unwrap_or(1600).clamp(240, 4320)),
    };
    match session::start(state.clone(), state.tailscale_ip, params).await {
        Ok(sdp) => Ok(Json(SessionResponse { sdp })),
        Err(e) => {
            tracing::warn!("session setup failed: {e:#}");
            Err((StatusCode::BAD_REQUEST, format!("{e:#}")))
        }
    }
}
