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

/// `tailscale whois` returns a raw control-plane node, which is shaped
/// differently from the nodes in `tailscale status`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoIs {
    node: Option<WhoIsNode>,
    user_profile: Option<UserProfile>,
}

#[derive(Debug, Clone, Deserialize)]
struct WhoIsNode {
    #[serde(rename = "User", default)]
    user: i64,
}

pub async fn status() -> Result<Status> {
    run_json(&["status", "--json"]).await
}

impl Status {
    pub fn is_running(&self) -> bool {
        self.backend_state == "Running" && self.self_node.is_some()
    }

    /// The tailnet user that owns this machine.
    pub fn owner(&self) -> Option<i64> {
        self.self_node.as_ref().map(|n| n.user_id)
    }
}

impl Node {
    pub fn ipv4(&self) -> Option<IpAddr> {
        self.tailscale_ips.iter().copied().find(IpAddr::is_ipv4)
    }

    /// MagicDNS name without the trailing dot.
    pub fn fqdn(&self) -> &str {
        self.dns_name.trim_end_matches('.')
    }

    /// Short machine name as MagicDNS resolves it (lowercase, first label).
    pub fn short_name(&self) -> String {
        let fqdn = self.fqdn();
        let label = fqdn.split('.').next().filter(|l| !l.is_empty()).unwrap_or(&self.host_name);
        label.to_ascii_lowercase()
    }
}

/// Decides whether a peer may connect: it must belong to the same tailnet
/// user as this machine. Results are cached briefly to keep requests cheap.
pub struct Gate {
    owner: i64,
    cache: Mutex<HashMap<IpAddr, (bool, Instant)>>,
}

impl Gate {
    pub fn new(status: &Status) -> Result<Self> {
        let owner = status.owner().context("tailscale has no self node")?;
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
                let user = w.user_profile.map(|u| u.id).or(w.node.map(|n| n.user));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_whois() {
        let json = r#"{"Node":{"ID":1,"Name":"pc.tail.ts.net.","User":42},"UserProfile":{"ID":42,"LoginName":"x"},"CapMap":null}"#;
        let w: WhoIs = serde_json::from_str(json).unwrap();
        assert_eq!(w.user_profile.unwrap().id, 42);
        assert_eq!(w.node.unwrap().user, 42);
    }
}
