//! Files sent from the client, saved in the Downloads folder of whoever is
//! signed in to this PC.
//!
//! Each file comes on a data channel of its own: a JSON [`FileHeader`], then
//! the bytes as binary messages. Once the last byte is written the host
//! answers with a [`FileReply`] and the client closes the channel. A file
//! whose channel closes early is deleted.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use conductor_proto::files::create_unique;
use conductor_proto::{FileHeader, FileReply};
use str0m::channel::ChannelId;
use windows::Win32::Foundation::{CloseHandle, ERROR_PRIVILEGE_NOT_HELD, HANDLE};
use windows::Win32::Security::{ImpersonateLoggedOnUser, RevertToSelf};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::HRESULT;

enum Transfer {
    /// Waiting for the header.
    Starting,
    Receiving { path: PathBuf, file: BufWriter<File>, left: u64 },
    /// Answered; waiting for the client to close the channel.
    Answered,
}

/// The files arriving right now, by channel.
#[derive(Default)]
pub struct Incoming(HashMap<ChannelId, Transfer>);

impl Incoming {
    pub fn open(&mut self, id: ChannelId) {
        self.0.insert(id, Transfer::Starting);
    }

    pub fn has(&self, id: ChannelId) -> bool {
        self.0.contains_key(&id)
    }

    /// Takes one message from a file channel. Returns the answer to send once
    /// the file is saved or has failed.
    pub fn receive(&mut self, id: ChannelId, binary: bool, data: &[u8]) -> Option<FileReply> {
        let transfer = self.0.get_mut(&id)?;
        match step(transfer, binary, data) {
            Ok(None) => None,
            Ok(Some(name)) => {
                tracing::info!("received {name}");
                *transfer = Transfer::Answered;
                Some(FileReply::Saved { name })
            }
            Err(e) => {
                tracing::warn!("receiving a file: {e:#}");
                discard(std::mem::replace(transfer, Transfer::Answered));
                Some(FileReply::Failed { message: format!("{e:#}") })
            }
        }
    }

    /// The channel closed. A file that didn't arrive in full is deleted.
    pub fn close(&mut self, id: ChannelId) {
        if let Some(transfer) = self.0.remove(&id) {
            discard(transfer);
        }
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        for (_, transfer) in self.0.drain() {
            discard(transfer);
        }
    }
}

/// Returns the saved file's name once its last byte is written.
fn step(transfer: &mut Transfer, binary: bool, data: &[u8]) -> Result<Option<String>> {
    match transfer {
        Transfer::Starting => {
            if binary {
                bail!("the file arrived without its name");
            }
            let header: FileHeader = serde_json::from_slice(data).context("reading the file's name and size")?;
            let (path, file) = create(&header.name)?;
            *transfer = Transfer::Receiving { path, file: BufWriter::with_capacity(1 << 20, file), left: header.size };
        }
        Transfer::Receiving { path, file, left } => {
            if !binary || data.len() as u64 > *left {
                bail!("more arrived than the file's size");
            }
            file.write_all(data).with_context(|| format!("writing {}", path.display()))?;
            *left -= data.len() as u64;
        }
        Transfer::Answered => return Ok(None),
    }
    match transfer {
        Transfer::Receiving { path, file, left: 0 } => {
            file.flush().with_context(|| format!("writing {}", path.display()))?;
            Ok(Some(path.file_name().unwrap_or_default().to_string_lossy().into_owned()))
        }
        _ => Ok(None),
    }
}

fn discard(transfer: Transfer) {
    if let Transfer::Receiving { path, file, .. } = transfer {
        // Closed first, or it can't be deleted.
        drop(file.into_parts());
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("couldn't delete the unfinished {}: {e}", path.display());
        }
    }
}

/// Creates a new file in the Downloads folder, as the signed-in user so it's
/// theirs and lands only where they could put it themselves.
fn create(name: &str) -> Result<(PathBuf, File)> {
    let user = User::signed_in()?;
    let folder = user.downloads()?;
    let _as_user = user.impersonate()?;
    create_unique(&folder, name).with_context(|| format!("saving {name} in {}", folder.display()))
}

/// Whoever is signed in to the session this process runs in.
struct User(Option<HANDLE>);

impl User {
    fn signed_in() -> Result<Self> {
        let mut session = 0;
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }.context("finding this session")?;
        let mut token = HANDLE::default();
        match unsafe { WTSQueryUserToken(session, &mut token) } {
            Ok(()) => Ok(Self(Some(token))),
            // Not running as the service, e.g. started by hand to try something:
            // this process is already that user.
            Err(e) if e.code() == HRESULT::from_win32(ERROR_PRIVILEGE_NOT_HELD.0) => Ok(Self(None)),
            Err(e) => Err(e).context("nobody is signed in to this PC"),
        }
    }

    fn downloads(&self) -> Result<PathBuf> {
        unsafe {
            let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, self.0)
                .context("finding the Downloads folder")?;
            let path = p.to_string();
            CoTaskMemFree(Some(p.0 as *const _));
            Ok(Path::new(&path.context("finding the Downloads folder")?).to_path_buf())
        }
    }

    /// Acts as the user on this thread until the guard is dropped.
    fn impersonate(&self) -> Result<Option<Impersonation>> {
        let Some(token) = self.0 else { return Ok(None) };
        unsafe { ImpersonateLoggedOnUser(token) }.context("acting as the signed-in user")?;
        Ok(Some(Impersonation))
    }
}

impl Drop for User {
    fn drop(&mut self) {
        if let Some(token) = self.0 {
            unsafe { _ = CloseHandle(token) };
        }
    }
}

struct Impersonation;

impl Drop for Impersonation {
    fn drop(&mut self) {
        unsafe { _ = RevertToSelf() };
    }
}
