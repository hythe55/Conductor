//! Wake-on-LAN details of the owner's PCs, remembered from when they were
//! last online: once a PC is asleep it can't say how to wake it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use conductor_proto::WakeInfo;

#[derive(Default)]
pub struct WakeMemory {
    path: Option<PathBuf>,
    /// By Tailscale node ID.
    known: Mutex<HashMap<String, WakeInfo>>,
}

impl WakeMemory {
    /// Remembers in a JSON file, so the details outlast restarts.
    pub fn open(path: PathBuf) -> Self {
        let known = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("{} is invalid, starting over: {e}", path.display());
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        };
        Self { path: Some(path), known: Mutex::new(known) }
    }

    pub(crate) fn remember(&self, id: &str, info: &WakeInfo) {
        let mut known = self.known.lock().unwrap();
        if known.get(id) == Some(info) {
            return;
        }
        known.insert(id.to_string(), info.clone());
        if let Some(path) = &self.path {
            if let Err(e) = save(path, &known) {
                tracing::warn!("saving {}: {e}", path.display());
            }
        }
    }

    pub(crate) fn recall(&self, id: &str) -> Option<WakeInfo> {
        self.known.lock().unwrap().get(id).cloned()
    }
}

fn save(path: &Path, known: &HashMap<String, WakeInfo>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(known)?)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_proto::WakeAdapter;

    #[test]
    fn remembers_across_restarts() {
        let path = std::env::temp_dir().join(format!("conductor-wake-{}.json", std::process::id()));
        let info = WakeInfo {
            adapters: vec![WakeAdapter { mac: "02:00:00:00:00:01".into(), address: [10, 0, 0, 5].into(), prefix: 24 }],
            verified: true,
        };
        WakeMemory::open(path.clone()).remember("n1", &info);
        let reopened = WakeMemory::open(path.clone());
        _ = fs::remove_file(&path);
        assert_eq!(reopened.recall("n1"), Some(info));
        assert_eq!(reopened.recall("n2"), None);
    }
}
