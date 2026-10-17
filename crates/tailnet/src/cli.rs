//! Thin wrapper over the `tailscale` CLI.
//!
//! The CLI talks to the local tailscaled, which already knows who every peer
//! is, so the host never needs its own accounts or passwords.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use conductor_proto::Route;
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
    /// When the coordination server last heard from an offline machine
    /// (RFC 3339). Tailscale reports year 1 for machines that are online.
    #[serde(default)]
    pub last_seen: Option<String>,
    /// Where traffic to the peer goes when it goes straight there. Empty
    /// while it's relayed, or when there's been no traffic lately.
    #[serde(default)]
    pub cur_addr: String,
    /// Code of the region of Tailscale's relay servers the peer uses, e.g. `nyc`.
    #[serde(default)]
    pub relay: String,
    /// Set while another device on the tailnet relays traffic to the peer.
    #[serde(default)]
    pub peer_relay: String,
    /// When this machine and the peer last agreed on keys for the tunnel
    /// (RFC 3339). Tailscale reports year 1 when they never have.
    #[serde(default)]
    pub last_handshake: Option<String>,
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

    /// `last_seen`, unless it's Tailscale's zero time.
    pub fn last_seen(&self) -> Option<&str> {
        self.last_seen.as_deref().filter(|t| !t.starts_with("0001-"))
    }

    /// Whether the peer has answered over the tunnel in the last few minutes.
    /// Keys are agreed again every two minutes while traffic flows, and before
    /// any traffic after that, so a peer that's awake and was just sent
    /// something always has a recent handshake.
    pub fn answered_lately(&self) -> bool {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        self.last_handshake.as_deref().and_then(unix_seconds).is_some_and(|t| now - t < 180)
    }

    /// How traffic to this peer travels right now.
    pub fn route(&self) -> Route {
        if !self.cur_addr.is_empty() {
            Route::Direct
        } else if !self.peer_relay.is_empty() {
            Route::PeerRelay
        } else {
            Route::Relay { region: self.relay.clone() }
        }
    }

    /// Short machine name as MagicDNS resolves it (lowercase, first label).
    pub fn short_name(&self) -> String {
        let fqdn = self.fqdn();
        let label = fqdn.split('.').next().filter(|l| !l.is_empty()).unwrap_or(&self.host_name);
        label.to_ascii_lowercase()
    }
}

/// Seconds since 1970 for an RFC 3339 time such as `2026-10-17T11:53:54.0554147-04:00`.
fn unix_seconds(t: &str) -> Option<i64> {
    let num = |s: &str| s.parse::<i64>().ok();
    let (date, rest) = t.split_once('T')?;
    let mut ymd = date.splitn(3, '-').map(num);
    let (y, m, d) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let (clock, offset) = match rest.find(['Z', 'z', '+', '-']) {
        Some(i) => rest.split_at(i),
        None => return None,
    };
    let clock = clock.split('.').next()?;
    let mut hms = clock.splitn(3, ':').map(num);
    let (h, min, s) = (hms.next()??, hms.next()??, hms.next()??);
    let offset = match offset.as_bytes().first() {
        Some(b'+' | b'-') => {
            let (oh, om) = offset[1..].split_once(':')?;
            let secs = num(oh)? * 3600 + num(om)? * 60;
            if offset.starts_with('-') { -secs } else { secs }
        }
        _ => 0,
    };
    // Days from 1970-01-01 to y-m-d in the proleptic Gregorian calendar.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + min * 60 + s - offset)
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

    #[test]
    fn times() {
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_seconds("2026-10-17T15:53:54.0554147Z"), Some(1_792_252_434));
        assert_eq!(unix_seconds("2026-10-17T11:53:54.0554147-04:00"), Some(1_792_252_434));
        assert_eq!(unix_seconds("2000-02-29T12:00:00+05:30"), Some(951_805_800));
        assert!(unix_seconds("0001-01-01T00:00:00Z").unwrap() < 0);
        assert_eq!(unix_seconds("yesterday"), None);
    }

    #[test]
    fn routes() {
        let node = |extra: &str| -> Node { serde_json::from_str(&format!(r#"{{"HostName":"pc"{extra}}}"#)).unwrap() };
        assert_eq!(node(r#","CurAddr":"192.168.1.2:41641","Relay":"nyc""#).route(), Route::Direct);
        assert_eq!(node(r#","CurAddr":"","Relay":"nyc""#).route(), Route::Relay { region: "nyc".into() });
        assert_eq!(node(r#","CurAddr":"","PeerRelay":"100.64.0.9:7777:3","Relay":"nyc""#).route(), Route::PeerRelay);
    }
}
