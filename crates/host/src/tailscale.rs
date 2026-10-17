//! Thin wrapper over the `tailscale` CLI.
//!
//! The CLI talks to the local tailscaled, which already knows who every peer
//! is, so the host never needs its own accounts or passwords.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

const WHOIS_TTL: Duration = Duration::from_secs(60);

fn cli_path() -> PathBuf {
    let installed = PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe");
    if installed.exists() { installed } else { PathBuf::from("tailscale") }
}

async fn run_json<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T> {
    let mut cmd = Command::new(cli_path());
    cmd.args(args).kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .context("tailscale CLI timed out")?
        .context("running tailscale CLI")?;
    if !out.status.success() {
        bail!("tailscale {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    serde_json::from_slice(&out.stdout).context("parsing tailscale CLI output")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Status {
    pub backend_state: String,
    #[serde(rename = "Self")]
    pub self_node: Option<Node>,
    #[serde(default)]
    pub peer: HashMap<String, Node>,
    #[serde(default)]
    pub user: HashMap<String, UserProfile>,
    #[serde(rename = "MagicDNSSuffix", default)]
    pub magic_dns_suffix: String,
    #[serde(default)]
    pub cert_domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Node {
    #[serde(rename = "ID", default)]
    pub id: String,
    pub host_name: String,
    #[serde(rename = "DNSName", default)]
    pub dns_name: String,
    #[serde(rename = "OS", default)]
    pub os: String,
    #[serde(rename = "UserID", default)]
    pub user_id: i64,
    #[serde(rename = "TailscaleIPs", default)]
    pub tailscale_ips: Vec<IpAddr>,
    #[serde(default)]
    pub online: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserProfile {
    #[serde(rename = "ID", default)]
    pub id: i64,
    #[serde(default)]
    pub login_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoIs {
    node: Option<Node>,
    user_profile: Option<UserProfile>,
}

pub async fn status() -> Result<Status> {
    run_json(&["status", "--json"]).await
}

/// Decides whether a peer may connect: it must belong to the same tailnet
/// user as this machine. Results are cached briefly to keep requests cheap.
pub struct Gate {
    owner: i64,
    cache: Mutex<HashMap<IpAddr, (bool, Instant)>>,
}

impl Gate {
    pub fn new(status: &Status) -> Result<Self> {
        let owner = status.self_node.as_ref().context("tailscale has no self node")?.user_id;
        Ok(Self { owner, cache: Mutex::new(HashMap::new()) })
    }

    pub async fn allows(&self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return true;
        }
        if let Some((ok, at)) = self.cache.lock().unwrap().get(&ip) {
            if at.elapsed() < WHOIS_TTL {
                return *ok;
            }
        }
        let ok = match run_json::<WhoIs>(&["whois", "--json", &ip.to_string()]).await {
            Ok(w) => {
                let user = w.user_profile.map(|u| u.id).or(w.node.map(|n| n.user_id));
                user == Some(self.owner)
            }
            Err(e) => {
                tracing::warn!("whois {ip} failed: {e:#}");
                false
            }
        };
        self.cache.lock().unwrap().insert(ip, (ok, Instant::now()));
        ok
    }
}
