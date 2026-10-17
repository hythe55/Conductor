use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::tailscale::Gate;

pub struct AppState {
    pub name: String,
    pub tailscale_ip: IpAddr,
    pub gate: Gate,
    pub web_dir: PathBuf,
    sessions: AtomicUsize,
}

impl AppState {
    pub fn new(name: String, tailscale_ip: IpAddr, gate: Gate, web_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self { name, tailscale_ip, gate, web_dir, sessions: AtomicUsize::new(0) })
    }

    pub fn session_count(&self) -> usize {
        self.sessions.load(Ordering::Relaxed)
    }

    pub fn session_started(self: &Arc<Self>) -> SessionGuard {
        self.sessions.fetch_add(1, Ordering::Relaxed);
        SessionGuard(self.clone())
    }
}

pub struct SessionGuard(Arc<AppState>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}
