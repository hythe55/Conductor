//! Machine-wide settings and log files under %ProgramData%\Conductor.

use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("Conductor")
}

/// Opens `name` in the log folder for appending, starting over once it gets large.
pub fn log_file(name: &str) -> Result<File> {
    let dir = data_dir().join("logs");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(name);
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        _ = fs::rename(&path, path.with_extension("old.log"));
    }
    OpenOptions::new().create(true).append(true).open(&path).with_context(|| format!("opening {}", path.display()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Other devices may control this PC.
    pub hosting: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { hosting: true }
    }
}

impl Settings {
    fn path() -> PathBuf {
        data_dir().join("settings.json")
    }

    pub fn load() -> Self {
        match fs::read(Self::path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("settings.json is invalid, using defaults: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        fs::create_dir_all(data_dir())?;
        let tmp = Self::path().with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(&tmp, Self::path())?;
        Ok(())
    }
}
