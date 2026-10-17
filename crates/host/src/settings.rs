//! Machine-wide settings and log files under %ProgramData%\Conductor.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
};
use windows::core::{BOOL, HSTRING, w};

const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("Conductor")
}

/// Where downloaded updates wait to be installed.
pub fn updates_dir() -> PathBuf {
    data_dir().join("updates")
}

/// Makes sure only SYSTEM and administrators can change the data folder, since
/// the service runs installers from it and writes logs there as SYSTEM. By
/// default any user may create files under %ProgramData%. A folder that someone
/// else created first, or a link to somewhere else, is moved aside and replaced.
/// Returns a note for the log when something had to be fixed.
pub fn secure_data_dir() -> Result<Option<String>> {
    let dir = data_dir();
    let sd = Descriptor::new()?;
    let mut note = None;
    match trusted(&dir) {
        Some(true) => {
            let dacl = sd.dacl()?;
            unsafe {
                SetNamedSecurityInfoW(
                    &HSTRING::from(dir.as_path()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(dacl),
                    None,
                )
                .ok()
                .with_context(|| format!("securing {}", dir.display()))?;
            }
            return Ok(None);
        }
        Some(false) => {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let aside = dir.with_file_name(format!("Conductor.untrusted-{stamp}"));
            fs::rename(&dir, &aside).with_context(|| format!("moving aside {}", dir.display()))?;
            note = Some(format!("{} wasn't created by Conductor; moved it to {}", dir.display(), aside.display()));
        }
        None => {}
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0.0,
        bInheritHandle: false.into(),
    };
    unsafe { CreateDirectoryW(&HSTRING::from(dir.as_path()), Some(&sa)) }
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(note)
}

/// Whether `dir` is a real folder owned by SYSTEM or administrators, or `None`
/// if it doesn't exist.
fn trusted(dir: &Path) -> Option<bool> {
    let path = HSTRING::from(dir);
    let attributes = unsafe { GetFileAttributesW(&path) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return None;
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Some(false);
    }
    let mut owner = PSID::default();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let err = unsafe {
        GetNamedSecurityInfoW(&path, SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, &mut sd)
    };
    if err.is_err() {
        return Some(false);
    }
    let ok = unsafe { IsWellKnownSid(owner, WinLocalSystemSid).as_bool() || IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool() };
    unsafe { LocalFree(Some(HLOCAL(sd.0))) };
    Some(ok)
}

/// SYSTEM and administrators have full control, other users can read. Nothing
/// is inherited from %ProgramData%.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn new() -> Result<Self> {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)"),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .context("building data folder permissions")?;
        }
        Ok(Self(sd))
    }

    fn dacl(&self) -> Result<*const ACL> {
        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut dacl = std::ptr::null_mut();
        unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted)? };
        if !present.as_bool() || dacl.is_null() {
            bail!("data folder permissions have no DACL");
        }
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
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
    /// Install new releases without asking.
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { hosting: true, auto_update: true }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_trust() {
        let dir = std::env::temp_dir().join(format!("conductor-trust-{}", std::process::id()));
        assert_eq!(trusted(&dir), None);
        fs::create_dir(&dir).unwrap();
        // Created by the user running the tests, not SYSTEM or administrators.
        let owned_by_user = trusted(&dir);
        fs::remove_dir(&dir).unwrap();
        assert_eq!(owned_by_user, Some(false));
        assert_eq!(trusted(Path::new(r"C:\Windows\System32")), Some(false), "owned by TrustedInstaller");
        assert_eq!(trusted(Path::new(r"C:\ProgramData")), Some(true), "owned by SYSTEM");
    }

    #[test]
    fn folder_permissions_parse() {
        let sd = Descriptor::new().unwrap();
        assert!(!sd.dacl().unwrap().is_null());
    }
}
