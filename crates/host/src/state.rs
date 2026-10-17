use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use conductor_proto::{Device, Route};
use conductor_tailnet::{Gate, Status};
use tokio::sync::Mutex;

/// How long a device list stays fresh. The list is only built when a client
/// has the device list open, so idle hosts never probe anyone.
const DEVICES_TTL: Duration = Duration::from_secs(5);
const STATUS_TTL: Duration = Duration::from_secs(30);
const ROUTE_TTL: Duration = Duration::from_secs(5);

pub struct AppState {
    /// Display name, e.g. `MSI`.
    pub name: String,
    /// MagicDNS short name and full name, lowercase.
    pub dns_label: String,
    pub fqdn: String,
    pub tailscale_ip: IpAddr,
    pub gate: Gate,
    pub web_dir: PathBuf,
    /// Sessions in progress, by id, with the name of the device on the other end.
    sessions: std::sync::Mutex<Vec<(u64, String)>>,
    next_session: AtomicU64,
    status: Mutex<(Status, Instant)>,
    devices: Mutex<Option<(Vec<Device>, Instant)>>,
    /// Names and addresses of the owner's own devices, lowercase.
    own_hosts: RwLock<HashSet<String>>,
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
            sessions: std::sync::Mutex::new(Vec::new()),
            next_session: AtomicU64::new(1),
            own_hosts: RwLock::new(own_hosts(&status)),
            status: Mutex::new((status, Instant::now())),
            devices: Mutex::new(None),
        });
        Ok(state)
    }

    pub fn session_started(self: &Arc<Self>, controller: String) -> SessionGuard {
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        self.sessions.lock().unwrap().push((id, controller));
        SessionGuard(self.clone(), id)
    }

    /// Names of the devices with a session open, in the order they connected.
    pub fn controllers(&self) -> Vec<String> {
        self.sessions.lock().unwrap().iter().map(|(_, name)| name.clone()).collect()
    }

    async fn status(&self) -> Status {
        self.status_within(STATUS_TTL).await
    }

    async fn status_within(&self, max_age: Duration) -> Status {
        let mut cached = self.status.lock().await;
        if cached.1.elapsed() > max_age {
            match conductor_tailnet::status().await {
                Ok(s) if s.is_running() => {
                    *self.own_hosts.write().unwrap() = own_hosts(&s);
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

    /// The tailnet name of the device at `ip`, for showing people who's connected.
    pub async fn device_name(&self, ip: IpAddr) -> String {
        if ip.is_loopback() {
            return self.name.clone();
        }
        let status = self.status_within(Duration::from_secs(5)).await;
        status
            .self_node
            .iter()
            .chain(status.peer.values())
            .find(|n| n.tailscale_ips.contains(&ip))
            .map(|n| n.host_name.clone())
            .unwrap_or_else(|| ip.to_string())
    }

    /// How traffic to the device at `ip` travels, when it's a peer on the tailnet.
    pub async fn route_to(&self, ip: IpAddr) -> Option<Route> {
        let status = self.status_within(ROUTE_TTL).await;
        status.peer.values().find(|n| n.tailscale_ips.contains(&ip)).map(|n| n.route())
    }

    /// Whether `host` (lowercase, no port) names one of the owner's devices.
    /// A miss refreshes the device list, in case the device was just added.
    pub async fn is_own_device(&self, host: &str) -> bool {
        if self.own_hosts.read().unwrap().contains(host) {
            return true;
        }
        self.status_within(Duration::from_secs(5)).await;
        self.own_hosts.read().unwrap().contains(host)
    }
}

fn own_hosts(status: &Status) -> HashSet<String> {
    let owner = status.owner();
    let mut hosts = HashSet::new();
    for node in status.self_node.iter().chain(status.peer.values()).filter(|n| Some(n.user_id) == owner) {
        hosts.insert(node.short_name());
        hosts.insert(node.fqdn().to_ascii_lowercase());
        hosts.extend(node.tailscale_ips.iter().map(|ip| ip.to_string()));
    }
    hosts.remove("");
    hosts
}

pub struct SessionGuard(Arc<AppState>, u64);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.sessions.lock().unwrap().retain(|(id, _)| *id != self.1);
    }
}
