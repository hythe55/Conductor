//! Keeps Conductor up to date from the project's GitHub releases.
//!
//! Every release publishes `latest.json`, naming the version and the installer
//! with its minisign signature. The service downloads the installer, checks the
//! signature against the key built into this program, and only then runs it.
//! The signed comment names the installer's file, so an older signed release
//! can't be passed off as a newer one.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;

use crate::https::{self, HttpStatus};
use crate::settings::{data_dir, secure_data_dir, updates_dir};

const MANIFEST_URL: &str = "https://github.com/hythe55/Conductor/releases/latest/download/latest.json";
const DOWNLOAD_PREFIX: &str = "https://github.com/hythe55/Conductor/releases/download/";
const PUBLIC_KEY: &str = include_str!("../keys/updater.pub");
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_INSTALLER: usize = 64 * 1024 * 1024;
/// The desktop app's executable, which the installer closes while it runs.
pub const APP_EXE: &str = "conductor-app.exe";
/// The app is reopened after an update only if the update finished this soon.
const REOPEN_WITHIN: Duration = Duration::from_secs(15 * 60);

#[derive(Deserialize)]
struct Manifest {
    version: String,
    windows: Asset,
}

#[derive(Deserialize)]
struct Asset {
    url: String,
    /// The text of the installer's `.sig` file.
    signature: String,
}

/// A newer release, downloaded and verified.
pub struct Update {
    pub version: String,
    installer: PathBuf,
}

/// Looks for a newer release, and downloads and verifies it if there is one.
/// Blocks for as long as the download takes.
pub fn fetch() -> Result<Option<Update>> {
    let manifest = match https::get(MANIFEST_URL, MAX_MANIFEST) {
        Ok(bytes) => bytes,
        // Nothing has been released yet.
        Err(e) if e.downcast_ref::<HttpStatus>().is_some_and(|s| s.0 == 404) => return Ok(None),
        Err(e) => return Err(e),
    };
    let manifest: Manifest = serde_json::from_slice(&manifest).context("reading latest.json")?;
    if !is_newer(&manifest.version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    if !manifest.windows.url.starts_with(DOWNLOAD_PREFIX) {
        bail!("latest.json points somewhere unexpected: {}", manifest.windows.url);
    }
    let bytes = https::get(&manifest.windows.url, MAX_INSTALLER)?;
    let file = installer_name(&manifest.version);
    verify(PUBLIC_KEY, &bytes, &manifest.windows.signature, &file)?;
    let installer = save(&bytes, &file)?;
    tracing::info!("downloaded and verified Conductor {}", manifest.version);
    Ok(Some(Update { version: manifest.version, installer }))
}

fn installer_name(version: &str) -> String {
    format!("Conductor_{version}_x64-setup.exe")
}

fn verify(key: &str, bytes: &[u8], signature: &str, file: &str) -> Result<()> {
    let key = PublicKey::decode(key).context("reading the update key")?;
    let signature = Signature::decode(signature).context("reading the installer's signature")?;
    key.verify(bytes, &signature, false).context("the installer's signature doesn't match")?;
    let expected = format!("file:{file}");
    if !signature.trusted_comment().split('\t').any(|field| field == expected) {
        bail!("the signature is for a different file: {}", signature.trusted_comment());
    }
    Ok(())
}

/// Writes the installer to a fresh folder that only SYSTEM and administrators
/// can change, so nobody can swap it before it runs.
fn save(bytes: &[u8], file: &str) -> Result<PathBuf> {
    secure_data_dir()?;
    let dir = updates_dir();
    if fs::symlink_metadata(&dir).is_ok() {
        fs::remove_dir_all(&dir).with_context(|| format!("clearing {}", dir.display()))?;
    }
    fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(file);
    let mut out = OpenOptions::new().write(true).create_new(true).open(&path)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    Ok(path)
}

/// Starts the installer and waits for it. When it works, the installer stops
/// this service and starts the new one, so this only returns on failure.
pub fn install(update: &Update) -> Result<()> {
    let sessions = app_sessions();
    if !sessions.is_empty() {
        let list: Vec<String> = sessions.iter().map(u32::to_string).collect();
        fs::write(reopen_marker(), list.join(",")).context("remembering to reopen the app")?;
    }
    tracing::info!("installing Conductor {}", update.version);
    // /UPDATE keeps the user's shortcuts as they are.
    let status = Command::new(&update.installer)
        .args(["/S", "/UPDATE"])
        .status()
        .context("starting the installer")?;
    _ = fs::remove_file(reopen_marker());
    bail!("the installer stopped with {status}")
}

fn reopen_marker() -> PathBuf {
    data_dir().join("reopen-app")
}

/// Runs when the service starts: removes old downloads and, right after an
/// update, reopens the app wherever the installer closed it.
pub fn after_start(app: PathBuf) {
    _ = fs::remove_dir_all(updates_dir());
    let marker = reopen_marker();
    let Ok(meta) = fs::metadata(&marker) else { return };
    let recent = meta.modified().ok().and_then(|t| SystemTime::now().duration_since(t).ok()) < Some(REOPEN_WITHIN);
    let sessions = fs::read_to_string(&marker).unwrap_or_default();
    _ = fs::remove_file(&marker);
    if !recent {
        return;
    }
    for session in sessions.split(',').filter_map(|s| s.trim().parse::<u32>().ok()) {
        match crate::spawn::spawn_as_user(session, &app) {
            Ok(()) => tracing::info!("reopened the app in session {session}"),
            Err(e) => tracing::warn!("reopening the app in session {session}: {e:#}"),
        }
    }
}

/// Sessions where the Conductor app is open.
pub fn app_sessions() -> Vec<u32> {
    sessions_running(APP_EXE)
}

/// Sessions with a process whose executable is named `exe`.
fn sessions_running(exe: &str) -> Vec<u32> {
    let mut sessions = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return sessions };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = std::ffi::OsString::from_wide(&entry.szExeFile[..len]);
            if name.to_string_lossy().eq_ignore_ascii_case(exe) {
                let mut session = 0;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut session).is_ok() && !sessions.contains(&session) {
                    sessions.push(session);
                }
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        _ = CloseHandle(snapshot);
    }
    sessions
}

/// Whether `candidate` is a later `major.minor.patch` than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
        let version = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("0.2.0-beta", "0.1.0"));
        assert!(!is_newer("0.2", "0.1.0"));
        assert!(!is_newer("0.2.0.1", "0.1.0"));
    }

    #[test]
    fn finds_running_programs() {
        let me = std::env::current_exe().unwrap();
        let mut session = 0;
        unsafe { ProcessIdToSessionId(std::process::id(), &mut session).unwrap() };
        assert!(sessions_running(&me.file_name().unwrap().to_string_lossy()).contains(&session));
        assert!(sessions_running("no-such-program.exe").is_empty());
    }

    #[test]
    fn built_in_key_parses() {
        PublicKey::decode(PUBLIC_KEY).unwrap();
    }

    // A throwaway key, not the release key, so nothing here is a valid release.
    const TEST_KEY: &str = "untrusted comment: minisign public key: 380BCF7537C14581
RWSBRcE3dc8LOJt5sSKKxUO/eu6NR7J3SOdDcoYCVzYVnTxpkVvP70YZ
";
    const TEST_SIG: &str = "untrusted comment: signature from tauri secret key
RUSBRcE3dc8LOGTLGxnzAoe7Hu0EP2LXBOx5kwU4ZSWQ53m1YNikaGLdEIlekEYwxChH6BbYXzC1UjpbZH7pASmNBOUJ+tCaaww=
trusted comment: timestamp:1792225204\tfile:Conductor_0.2.0_x64-setup.exe
ad8yauPINyhTIVzkVZuvzN3XRXUxo0qOQxs/TUQwWfjAxptEPVbWOCUktI9mOm30RlcVmWhjOMRBpRXoK2/oAQ==
";
    const TEST_FILE: &str = "Conductor_0.2.0_x64-setup.exe";

    #[test]
    fn signatures() {
        verify(TEST_KEY, b"pretend installer", TEST_SIG, TEST_FILE).unwrap();
        // Changed contents.
        assert!(verify(TEST_KEY, b"pretend installer!", TEST_SIG, TEST_FILE).is_err());
        // An older release's signature passed off as a newer one.
        assert!(verify(TEST_KEY, b"pretend installer", TEST_SIG, "Conductor_0.3.0_x64-setup.exe").is_err());
        // Signed by someone else.
        assert!(verify(PUBLIC_KEY, b"pretend installer", TEST_SIG, TEST_FILE).is_err());
        assert!(verify(TEST_KEY, b"pretend installer", "not a signature", TEST_FILE).is_err());
    }
}
