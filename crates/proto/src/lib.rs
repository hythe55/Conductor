//! Messages exchanged between a Conductor client and host.
//!
//! HTTP bodies and data-channel messages are JSON. Data-channel messages are
//! tagged with `t` so the web client can switch on a short string.

use serde::{Deserialize, Serialize};

/// Default TCP port for the host's HTTP API and web client.
pub const DEFAULT_PORT: u16 = 4280;

/// Wire protocol version. Bumped on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Named pipe the Conductor service listens on for the local app.
pub const CONTROL_PIPE: &str = r"\\.\pipe\conductor";

/// One JSON line sent by the local app to the service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ControlRequest {
    Status,
    #[serde(rename_all = "camelCase")]
    SetHosting { enabled: bool },
}

/// The service's reply to every [`ControlRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlStatus {
    /// Other devices may control this PC.
    pub hosting: bool,
    /// The agent that serves sessions is running right now.
    pub agent_running: bool,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub os: String,
    pub displays: Vec<DisplayInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    /// Stable for the lifetime of the display (Windows device name, e.g. `\\.\DISPLAY1`).
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
}

/// A machine on the tailnet that Conductor can show on the device list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Tailscale's stable node ID.
    pub id: String,
    /// Short machine name, e.g. `desktop`.
    pub name: String,
    /// Full MagicDNS name without the trailing dot, e.g. `desktop.tail1234.ts.net`.
    pub dns_name: String,
    /// Tailscale IPv4 address; what clients connect to.
    pub address: String,
    pub os: String,
    /// Tailscale sees the machine as connected.
    pub online: bool,
    /// Present when Conductor is running there and answered.
    pub host: Option<HostInfo>,
    /// The machine that produced this list.
    #[serde(default)]
    pub is_self: bool,
    /// The machine that asked for this list.
    #[serde(default)]
    pub is_you: bool,
}

/// Body of `POST /api/session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequest {
    /// SDP offer from the client's RTCPeerConnection.
    pub sdp: String,
    /// Display to start on. Primary display when omitted.
    #[serde(default)]
    pub display: Option<String>,
    /// Largest video size the client wants, in device pixels.
    #[serde(default)]
    pub max_width: Option<u32>,
    #[serde(default)]
    pub max_height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub sdp: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Client to host, over the `control` data channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ClientMsg {
    /// Absolute pointer position, normalized to the streamed display (0..=1).
    #[serde(rename_all = "camelCase")]
    MouseMove { x: f32, y: f32 },
    /// Relative pointer motion in host pixels.
    #[serde(rename_all = "camelCase")]
    MouseDelta { dx: i32, dy: i32 },
    #[serde(rename_all = "camelCase")]
    MouseButton { button: MouseButton, down: bool },
    /// Wheel delta in Windows units (120 = one notch).
    #[serde(rename_all = "camelCase")]
    Wheel { dx: i32, dy: i32 },
    /// Physical key, as a DOM `KeyboardEvent.code` value.
    #[serde(rename_all = "camelCase")]
    Key { code: String, down: bool },
    /// Literal text, for soft keyboards and IME output.
    #[serde(rename_all = "camelCase")]
    Text { text: String },
    /// Release every key and button the host thinks is held.
    ReleaseAll,
    #[serde(rename_all = "camelCase")]
    SelectDisplay { id: String },
    RequestKeyframe,
    /// Stop sending video while the client can't show it (minimized, screen off).
    #[serde(rename_all = "camelCase")]
    SetPaused { paused: bool },
}

/// Host to client, over the `control` data channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum HostMsg {
    #[serde(rename_all = "camelCase")]
    Hello { host: HostInfo, display: String },
    #[serde(rename_all = "camelCase")]
    Displays { displays: Vec<DisplayInfo>, active: String },
    #[serde(rename_all = "camelCase")]
    Stats { bitrate_kbps: u32, fps: u32, width: u32, height: u32, encoder: String },
    #[serde(rename_all = "camelCase")]
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_msg_wire_format() {
        let m: ClientMsg = serde_json::from_str(r#"{"t":"mouseMove","x":0.5,"y":0.25}"#).unwrap();
        assert!(matches!(m, ClientMsg::MouseMove { x, y } if x == 0.5 && y == 0.25));
        let m: ClientMsg =
            serde_json::from_str(r#"{"t":"mouseButton","button":"left","down":true}"#).unwrap();
        assert!(matches!(m, ClientMsg::MouseButton { button: MouseButton::Left, down: true }));
        let s = serde_json::to_string(&ClientMsg::ReleaseAll).unwrap();
        assert_eq!(s, r#"{"t":"releaseAll"}"#);
    }
}
