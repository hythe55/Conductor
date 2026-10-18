//! Commands that only make sense on a Windows PC running Tailscale.

use std::time::Duration;

use conductor_proto::{CONTROL_PIPE, ControlRequest, ControlStatus, Device};
use conductor_tailnet::WakeMemory;
use serde::Serialize;
use tauri::ipc::{InvokeBody, Request};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;

/// An error for people: a plain message, plus the raw cause behind "Details".
#[derive(Debug, Serialize)]
pub struct Problem {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
}

impl Problem {
    pub fn new(message: &str, details: impl ToString) -> Self {
        Self { message: message.into(), details: Some(details.to_string()) }
    }
}

const NO_SERVICE: &str = "Couldn't reach the Conductor service on this PC. Restart the PC, or reinstall Conductor.";

/// The owner's PCs on the tailnet, with this one marked as "you".
#[tauri::command]
pub async fn devices(memory: tauri::State<'_, WakeMemory>) -> Result<Vec<Device>, Problem> {
    let status = conductor_tailnet::status()
        .await
        .map_err(|e| Problem::new("Tailscale isn't running on this PC. Open Tailscale and sign in.", format!("{e:#}")))?;
    if !status.is_running() {
        return Err(Problem::new(
            "Tailscale isn't connected on this PC. Open Tailscale and sign in.",
            format!("Tailscale state: {}", status.backend_state),
        ));
    }
    let mut list = conductor_tailnet::discover(&status, None, &memory).await;
    for d in &mut list {
        d.is_you = d.is_self;
    }
    Ok(list)
}

/// Hosting state from the Conductor service, or `None` when the service
/// isn't installed (e.g. running a development build).
#[tauri::command]
pub async fn host_status() -> Result<Option<ControlStatus>, Problem> {
    match call(&ControlRequest::Status).await {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Problem::new(NO_SERVICE, e)),
    }
}

#[tauri::command]
pub async fn set_hosting(enabled: bool) -> Result<ControlStatus, Problem> {
    request(ControlRequest::SetHosting { enabled }).await
}

#[tauri::command]
pub async fn set_auto_update(enabled: bool) -> Result<ControlStatus, Problem> {
    request(ControlRequest::SetAutoUpdate { enabled }).await
}

#[tauri::command]
pub async fn set_previews(enabled: bool) -> Result<ControlStatus, Problem> {
    request(ControlRequest::SetPreviews { enabled }).await
}

#[tauri::command]
pub async fn check_for_updates() -> Result<ControlStatus, Problem> {
    request(ControlRequest::CheckForUpdates).await
}

#[tauri::command]
pub async fn install_update() -> Result<ControlStatus, Problem> {
    request(ControlRequest::InstallUpdate).await
}

async fn request(request: ControlRequest) -> Result<ControlStatus, Problem> {
    call(&request).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Problem::new("The Conductor service isn't installed. Reinstall Conductor to change this.", e)
        } else {
            Problem::new(NO_SERVICE, e)
        }
    })
}

async fn call(request: &ControlRequest) -> std::io::Result<ControlStatus> {
    let pipe = ClientOptions::new().open(CONTROL_PIPE)?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    let mut reader = BufReader::new(pipe);
    reader.get_mut().write_all(&line).await?;
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut reply))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"))??;
    Ok(serde_json::from_str(reply.trim())?)
}

/// Clipboard text bigger than this isn't shared; the host has the same limit.
const CLIPBOARD_MAX: usize = 120 * 1024;

/// Text on this PC's clipboard, to share with the PC being controlled.
#[tauri::command]
pub async fn read_clipboard() -> Option<String> {
    tauri::async_runtime::spawn_blocking(|| conductor_clipboard::read_text(CLIPBOARD_MAX).ok().flatten())
        .await
        .ok()
        .flatten()
}

const COULDNT_COPY: &str = "Couldn't copy to this PC's clipboard.";

/// Puts text copied on the PC being controlled on this PC's clipboard.
#[tauri::command]
pub async fn write_clipboard(text: String) -> Result<(), Problem> {
    tauri::async_runtime::spawn_blocking(move || conductor_clipboard::write_text(&text))
        .await
        .map_err(|e| Problem::new(COULDNT_COPY, e))?
        .map_err(|e| Problem::new(COULDNT_COPY, format!("{e:#}")))
}

/// Puts a picture copied on the PC being controlled, a PNG in the request
/// body, on this PC's clipboard. Returns the clipboard's sequence number
/// afterwards, so the app can tell whether it's still there.
#[tauri::command]
pub async fn write_clipboard_image(request: Request<'_>) -> Result<u32, Problem> {
    let InvokeBody::Raw(png) = request.body() else {
        return Err(Problem::new(COULDNT_COPY, "the picture arrived as JSON"));
    };
    let png = png.clone();
    tauri::async_runtime::spawn_blocking(move || {
        conductor_clipboard::write_image(&png).map(|()| conductor_clipboard::sequence())
    })
    .await
    .map_err(|e| Problem::new(COULDNT_COPY, e))?
    .map_err(|e| Problem::new(COULDNT_COPY, format!("{e:#}")))
}

/// Changes whenever something new is copied on this PC.
#[tauri::command]
pub fn clipboard_sequence() -> u32 {
    conductor_clipboard::sequence()
}
