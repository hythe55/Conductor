//! HTTP API and web client, reachable only on the tailnet and localhost.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use conductor_proto::{
    Device, HostInfo, PROTOCOL_VERSION, PowerRequest, PowerSchedule, ScheduleRequest, SessionRequest, SessionResponse,
    WakeInfo, WakeRequest,
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

use crate::ask::Answer;
use crate::session;
use crate::settings::Settings;
use crate::state::AppState;

/// Origins of the bundled desktop/Android app and the web dev server.
const APP_ORIGINS: &[&str] =
    &["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost", "http://localhost:5173"];

pub fn router(state: Arc<AppState>) -> Router {
    let index = state.web_dir.join("index.html");
    // `gate` runs first and has already refused origins that aren't allowed.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    Router::new()
        .route("/api/info", get(info))
        .route("/api/devices", get(devices))
        .route("/api/session", post(create_session))
        .route("/api/wake", post(wake))
        .route("/api/wake/confirm", post(confirm_wake))
        .route("/api/power", post(power))
        .route("/api/preview", get(preview))
        .route("/api/schedule", post(set_schedule))
        .fallback_service(ServeDir::new(&state.web_dir).fallback(ServeFile::new(index)))
        .layer(middleware::from_fn(revalidate_pages))
        .layer(cors)
        .layer(middleware::from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

/// Scripts and styles are named by content hash and fine to cache, but the page
/// that links them has to be checked each time, or after an update browsers keep
/// loading the old client.
async fn revalidate_pages(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let is_page = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("text/html"));
    if is_page {
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    res
}

/// Only this tailnet user's devices (and localhost) get in. The Host header
/// check stops DNS-rebinding pages from talking to us through the browser,
/// and the Origin check stops other websites open on a tailnet device.
async fn gate(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(host_name).unwrap_or_default();
    if !host_allowed(&state, &host) {
        return (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        if !origin_allowed(&state, origin).await {
            return (StatusCode::FORBIDDEN, "Requests from this website aren't allowed.").into_response();
        }
    }
    if !state.gate.allows(peer.ip()).await {
        return (StatusCode::FORBIDDEN, "This device isn't allowed to connect.").into_response();
    }
    next.run(req).await
}

/// Lowercase host from a `host[:port]` authority, without IPv6 brackets.
fn host_name(authority: &str) -> String {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else if authority.matches(':').count() > 1 {
        // A bare IPv6 address has no port.
        authority
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.to_ascii_lowercase()
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

fn host_allowed(state: &AppState, host: &str) -> bool {
    is_loopback(host)
        || host == state.tailscale_ip.to_string()
        || host == state.dns_label
        || host == state.fqdn
}

/// Browsers may call this API from the Conductor app itself or from the web
/// client served by any of the owner's machines. Everything else (including
/// websites open on a tailnet device, or other people's shared machines) is
/// refused.
async fn origin_allowed(state: &AppState, origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else { return false };
    if APP_ORIGINS.contains(&origin) {
        return true;
    }
    let Some(rest) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else {
        return false;
    };
    let host = host_name(rest);
    if is_loopback(&host) {
        // Pages served on this PC, such as the web client opened locally.
        return true;
    }
    state.is_own_device(&host).await
}

async fn info(State(state): State<Arc<AppState>>) -> Json<HostInfo> {
    Json(host_info(&state))
}

async fn devices(State(state): State<Arc<AppState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>) -> Json<Vec<Device>> {
    Json(state.devices(peer.ip()).await)
}

pub fn host_info(state: &AppState) -> HostInfo {
    let settings = Settings::load();
    HostInfo {
        name: state.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL_VERSION,
        os: "windows".into(),
        displays: crate::capture::displays().unwrap_or_default(),
        controllers: state.controllers(),
        wake: Some(crate::wake::info()),
        preview: settings.previews,
        schedule: Some(crate::schedule::load()),
        pen: true,
        paste: true,
        sharp: true,
        ask: settings.ask,
    }
}

async fn set_schedule(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<ScheduleRequest>,
) -> Result<Json<Vec<PowerSchedule>>, (StatusCode, String)> {
    tracing::info!("{} set this PC's schedule: {:?}", state.device_name(peer.ip()).await, req.schedule);
    crate::schedule::save(req.schedule).map(Json).map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))
}

/// A small picture of the main display, for device lists.
async fn preview() -> Response {
    if !Settings::load().previews {
        return (StatusCode::NOT_FOUND, "Previews are turned off on this PC.").into_response();
    }
    match crate::preview::take().await {
        Ok(jpeg) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "no-store")], jpeg).into_response(),
        Err(e) => {
            tracing::warn!("taking a preview: {e:#}");
            (StatusCode::SERVICE_UNAVAILABLE, format!("{e:#}")).into_response()
        }
    }
}

/// Wakes another of the owner's PCs on this PC's network.
async fn wake(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<WakeRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    tracing::info!("{} asked to wake a PC", state.device_name(peer.ip()).await);
    match crate::wake::send(&req.adapters).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => {
            tracing::warn!("waking: {e:#}");
            Err((StatusCode::CONFLICT, format!("{e:#}")))
        }
    }
}

/// Called after a test wake brings this PC back, so waking it is offered from now on.
async fn confirm_wake() -> Result<Json<WakeInfo>, (StatusCode, String)> {
    crate::wake::confirm().map(Json).map_err(|e| (StatusCode::CONFLICT, format!("{e:#}")))
}

async fn power(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<PowerRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    tracing::info!("{} asked this PC to {:?}", state.device_name(peer.ip()).await, req.action);
    match crate::power::act(req.action) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) => {
            tracing::warn!("{:?}: {e:#}", req.action);
            Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
        }
    }
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<SessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, String)> {
    let controller = state.device_name(peer.ip()).await;
    if Settings::load().ask && !state.allowed(peer.ip()) {
        let refused = |why: String| Err((StatusCode::CONFLICT, why));
        match crate::ask::ask(controller.clone(), peer.ip()).await {
            Some(Answer::Allowed) => {
                tracing::info!("someone at this PC let {controller} in");
                state.allow(peer.ip());
            }
            Some(Answer::Declined) => {
                tracing::info!("someone at this PC didn't let {controller} in");
                return refused(format!("Someone at {} didn’t allow it.", state.name));
            }
            Some(Answer::Unanswered) => {
                tracing::info!("nobody at this PC answered whether {controller} may connect");
                return refused(format!("Nobody at {} answered in time, so it wasn’t allowed.", state.name));
            }
            None => return refused(format!("Someone at {} is being asked already.", state.name)),
        }
    }
    let params = session::Params {
        controller,
        peer: peer.ip(),
        offer: req.sdp,
        display: req.display,
        max_size: (req.max_width.unwrap_or(2560).clamp(320, 7680), req.max_height.unwrap_or(1600).clamp(240, 4320)),
        sharp: req.sharp,
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
    fn host_names() {
        assert_eq!(host_name("MSI-1:4280"), "msi-1");
        assert_eq!(host_name("[::1]:4280"), "::1");
        assert_eq!(host_name("::1"), "::1");
        assert_eq!(host_name("pc.tail1234.ts.net"), "pc.tail1234.ts.net");
    }
}
