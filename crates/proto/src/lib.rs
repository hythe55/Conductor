//! Messages exchanged between a Conductor client and host.
//!
//! HTTP bodies and data-channel messages are JSON. Data-channel messages are
//! tagged with `t` so the web client can switch on a short string.

use serde::{Deserialize, Serialize};

pub mod files;

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
    #[serde(rename_all = "camelCase")]
    SetAutoUpdate { enabled: bool },
    CheckForUpdates,
    /// Install the downloaded update now, even if the app is open or someone is connected.
    InstallUpdate,
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
    /// Updates install on their own when nobody is using Conductor.
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default)]
    pub update: UpdateStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum UpdateStatus {
    /// No check since the service started.
    #[default]
    NotChecked,
    Checking,
    UpToDate,
    /// Downloaded and verified, waiting until nobody is using Conductor.
    #[serde(rename_all = "camelCase")]
    Ready { version: String },
    #[serde(rename_all = "camelCase")]
    Installing { version: String },
    #[serde(rename_all = "camelCase")]
    Failed {
        message: String,
        /// The update downloaded but didn't install, rather than the check failing.
        #[serde(default)]
        installing: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub os: String,
    pub displays: Vec<DisplayInfo>,
    /// Names of the devices controlling this PC right now.
    #[serde(default)]
    pub controllers: Vec<String>,
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
    /// For offline machines, when Tailscale last heard from them (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
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
    /// Text copied on the client, to put on this PC's clipboard.
    #[serde(rename_all = "camelCase")]
    Clipboard { text: String },
    /// Send the sound playing on this PC, or stop. Off until asked.
    #[serde(rename_all = "camelCase")]
    SetAudio { enabled: bool },
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
    /// Text that was just copied on this PC.
    #[serde(rename_all = "camelCase")]
    Clipboard { text: String },
    /// The pointer changed shape. A CSS cursor value, `none` when it's hidden.
    #[serde(rename_all = "camelCase")]
    Cursor { name: String },
    /// How the connection travels between the two devices, sent when that changes.
    #[serde(rename_all = "camelCase")]
    Route { route: Route },
    /// Files were just copied on this PC and can be fetched by `offer` on
    /// [`COPIED_CHANNEL`]s. Empty when the clipboard no longer holds files.
    #[serde(rename_all = "camelCase")]
    FilesCopied {
        offer: u32,
        files: Vec<CopiedFile>,
        /// Copied folders, which can't be fetched.
        folders: u32,
    },
}

/// How traffic between a client and host travels over the tailnet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Route {
    /// Straight between the two devices.
    Direct,
    /// Through another device on the tailnet.
    PeerRelay,
    /// Through one of Tailscale's relay servers, named by its region code (e.g. `nyc`).
    #[serde(rename_all = "camelCase")]
    Relay { region: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopiedFile {
    pub name: String,
    pub size: u64,
}

/// Label of the data channels that carry files from the client, one file each.
pub const FILE_CHANNEL: &str = "file";

/// First message on a file channel, as text. The file's bytes follow as binary messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHeader {
    /// The file's name on the client, without its folder.
    pub name: String,
    pub size: u64,
}

/// Label of the data channels that fetch files copied on the host, one file
/// each. The client sends a [`FileRequest`]; the host answers with a
/// [`FileHeader`] and the bytes, or a [`FileReply::Failed`].
pub const COPIED_CHANNEL: &str = "copied";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRequest {
    pub offer: u32,
    /// Position in the offer's file list.
    pub index: u32,
}

/// The host's answer on a file channel, after which the client closes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum FileReply {
    /// Saved in the Downloads folder under this name, which may differ from
    /// the one sent when a file of that name was already there.
    #[serde(rename_all = "camelCase")]
    Saved { name: String },
    #[serde(rename_all = "camelCase")]
    Failed { message: String },
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
