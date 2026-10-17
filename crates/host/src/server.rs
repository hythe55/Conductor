//! HTTP API and web client, reachable only on the tailnet and localhost.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use conductor_proto::{Device, HostInfo, PROTOCOL_VERSION, SessionRequest, SessionResponse};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use crate::session;
use crate::state::AppState;

/// Origins of the bundled desktop/Android app and the web dev server.
const APP_ORIGINS: &[&str] =
    &["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost", "http://localhost:5173"];

pub fn router(state: Arc<AppState>) -> Router {
    let index = state.web_dir.join("index.html");
    let origins = state.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, _| origin_allowed(&origins, origin)))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    Router::new()
        .route("/api/info", get(info))
        .route("/api/devices", get(devices))
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
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == state.tailscale_ip.to_string()
        || host == state.dns_label
        || host == state.fqdn
}

/// Browsers may call this API from the Conductor app itself or from the web
/// client served by any of the owner's machines. Everything else (including
/// arbitrary websites open on a tailnet device) is refused.
fn origin_allowed(state: &AppState, origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else { return false };
    if APP_ORIGINS.contains(&origin) {
        return true;
    }
    let Some(rest) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else {
        return false;
    };
    let host = match rest.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => rest,
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => is_tailscale_ip(ip),
        Err(_) => state.is_tailnet_name(&host),
    }
}

fn is_tailscale_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        // 100.64.0.0/10 (CGNAT range Tailscale assigns from).
        std::net::IpAddr::V4(v4) => v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64,
        // fd7a:115c:a1e0::/48
        std::net::IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

async fn info(State(state): State<Arc<AppState>>) -> Json<HostInfo> {
    Json(host_info(&state))
}

async fn devices(State(state): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>) -> Json<Vec<Device>> {
    Json(state.devices(peer.ip()).await)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_ranges() {
        assert!(is_tailscale_ip("100.101.102.103".parse().unwrap()));
        assert!(is_tailscale_ip("100.64.0.1".parse().unwrap()));
        assert!(!is_tailscale_ip("100.128.0.1".parse().unwrap()));
        assert!(!is_tailscale_ip("192.168.1.10".parse().unwrap()));
        assert!(is_tailscale_ip("fd7a:115c:a1e0::1".parse().unwrap()));
        assert!(!is_tailscale_ip("fd00::1".parse().unwrap()));
    }
}
