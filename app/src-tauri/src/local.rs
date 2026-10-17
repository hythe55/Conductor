//! Commands that only make sense on a Windows PC running Tailscale.

use std::time::Duration;

use conductor_proto::{CONTROL_PIPE, ControlRequest, ControlStatus, Device};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;

/// The owner's PCs on the tailnet, with this one marked as "you".
#[tauri::command]
pub async fn devices() -> Result<Vec<Device>, String> {
    let status = conductor_tailnet::status().await.map_err(|_| {
        "Tailscale isn't running on this PC. Open Tailscale and sign in, then try again.".to_string()
    })?;
    if !status.is_running() {
        return Err("Tailscale isn't connected. Open Tailscale and sign in, then try again.".into());
    }
    let mut list = conductor_tailnet::discover(&status, None).await;
    for d in &mut list {
        d.is_you = d.is_self;
    }
    Ok(list)
}

/// Hosting state from the Conductor service, or `None` when the service
/// isn't installed (e.g. running a development build).
#[tauri::command]
pub async fn host_status() -> Result<Option<ControlStatus>, String> {
    match call(&ControlRequest::Status).await {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Couldn't reach the Conductor service: {e}")),
    }
}

#[tauri::command]
pub async fn set_hosting(enabled: bool) -> Result<ControlStatus, String> {
    call(&ControlRequest::SetHosting { enabled }).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "The Conductor service isn't installed. Reinstall Conductor to turn this on.".into()
        } else {
            format!("Couldn't reach the Conductor service: {e}")
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
