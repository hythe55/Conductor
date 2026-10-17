use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use conductor_proto::Device;
use conductor_tailnet::{Gate, Status};
use tokio::sync::Mutex;

/// How long a device list stays fresh. The list is only built when a client
/// has the device list open, so idle hosts never probe anyone.
const DEVICES_TTL: Duration = Duration::from_secs(5);
const STATUS_TTL: Duration = Duration::from_secs(30);

pub struct AppState {
    /// Display name, e.g. `MSI`.
    pub name: String,
    /// MagicDNS short name and full name, lowercase.
    pub dns_label: String,
    pub fqdn: String,
    pub tailscale_ip: IpAddr,
    pub gate: Gate,
    pub web_dir: PathBuf,
    sessions: AtomicUsize,
    status: Mutex<(Status, Instant)>,
    devices: Mutex<Option<(Vec<Device>, Instant)>>,
    /// Names of the owner's machines, for checking request origins.
    peer_names: RwLock<(HashSet<String>, String)>,
}

impl AppState {
    pub fn new(status: Status, tailscale_ip: IpAddr, web_dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        let me = status.self_node.as_ref().expect("checked by caller");
        let state = Arc::new(Self {
            name: me.host_name.clone(),
            dns_label: me.short_name(),
            fqdn: me.fqdn().to_ascii_lowercase(),
            tailscale_ip,
            gate: Gate::new(&status)?,
            web_dir,
            sessions: AtomicUsize::new(0),
            peer_names: RwLock::new(peer_names(&status)),
            status: Mutex::new((status, Instant::now())),
            devices: Mutex::new(None),
        });
        Ok(state)
    }

    pub fn session_started(self: &Arc<Self>) -> SessionGuard {
        self.sessions.fetch_add(1, Ordering::Relaxed);
        SessionGuard(self.clone())
    }

    async fn status(&self) -> Status {
        let mut cached = self.status.lock().await;
        if cached.1.elapsed() > STATUS_TTL {
            match conductor_tailnet::status().await {
                Ok(s) if s.is_running() => {
                    *self.peer_names.write().unwrap() = peer_names(&s);
                    *cached = (s, Instant::now());
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("tailscale status: {e:#}"),
            }
        }
        cached.0.clone()
    }

    pub async fn devices(&self, requester: IpAddr) -> Vec<Device> {
        let mut cached = self.devices.lock().await;
        let devices = match &*cached {
            Some((list, at)) if at.elapsed() < DEVICES_TTL => list.clone(),
            _ => {
                let status = self.status().await;
                let list = conductor_tailnet::discover(&status, None).await;
                *cached = Some((list.clone(), Instant::now()));
                list
            }
        };
        let requester_ips = |d: &Device| d.address == requester.to_string();
        devices.into_iter().map(|d| Device { is_you: requester_ips(&d), ..d }).collect()
    }

    /// True for page origins served by one of the owner's machines.
    pub fn is_tailnet_name(&self, host: &str) -> bool {
        let names = self.peer_names.read().unwrap();
        let (labels, suffix) = &*names;
        labels.contains(host) || (!suffix.is_empty() && host.ends_with(&format!(".{suffix}")))
    }
}

fn peer_names(status: &Status) -> (HashSet<String>, String) {
    let owner = status.owner();
    let labels = status
        .self_node
        .iter()
        .chain(status.peer.values())
        .filter(|n| Some(n.user_id) == owner)
        .map(|n| n.short_name())
        .collect();
    (labels, status.magic_dns_suffix.to_ascii_lowercase())
}

pub struct SessionGuard(Arc<AppState>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}
