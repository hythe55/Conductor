//! Files moving between the client and this PC, each on a data channel of
//! its own.
//!
//! Files the client sends are saved in the Downloads folder of whoever is
//! signed in to this PC. A JSON [`FileHeader`] comes first, then the bytes as
//! binary messages. Once the last byte is written the host answers with a
//! [`FileReply`] and the client closes the channel. A file whose channel
//! closes early is deleted.
//!
//! Files copied on this PC are offered to the client, which fetches them one
//! by one with a [`FileRequest`]. The host answers with a [`FileHeader`] and
//! the bytes, or a [`FileReply::Failed`].

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::task::Poll;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use conductor_proto::files::create_unique;
use conductor_proto::{CopiedFile, FileHeader, FileReply, FileRequest, HostMsg};
use str0m::Rtc;
use str0m::channel::ChannelId;
use tokio::sync::mpsc;
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
            Ok(Some(file_name(path)))
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

/// Copied files waiting to go out stop being read ahead while this much of
/// them is queued on the connection. The connection holds more than this so
/// control messages always fit.
const QUEUED_MAX: usize = 1 << 20;
const CHUNK: usize = 64 * 1024;
/// How many copies stay fetchable, so a save that's under way can finish
/// after something else is copied.
const OFFERS_KEPT: usize = 8;
/// Roughly how much of a control message the list of copied files may take.
const OFFER_MAX_BYTES: usize = 128 * 1024;

/// A message for a file channel: whether it's binary, and its bytes.
type Message = (bool, Vec<u8>);

enum Fetch {
    /// Waiting for the client to say which file it wants.
    Starting,
    Reading(mpsc::Receiver<Message>),
    /// Everything is queued; waiting for the client to close the channel.
    Read,
}

/// Files copied on this PC, and the ones the client is fetching, by channel.
#[derive(Default)]
pub struct Outgoing {
    offers: VecDeque<(u32, Vec<PathBuf>)>,
    last_offer: u32,
    /// Files are on this PC's clipboard and the client knows about them.
    offering: bool,
    fetches: HashMap<ChannelId, Fetch>,
    /// There's room on the connection for more file data.
    room: bool,
}

impl Outgoing {
    /// Makes files just copied on this PC fetchable and returns the message
    /// that tells the client about them.
    pub fn offer(&mut self, paths: Vec<PathBuf>) -> HostMsg {
        let (paths, files, folders) = describe(paths);
        tracing::info!("copied on this PC: files {}, folders {folders}", files.len());
        self.last_offer += 1;
        let offer = self.last_offer;
        self.offering = !files.is_empty();
        if self.offering {
            self.offers.push_back((offer, paths));
            if self.offers.len() > OFFERS_KEPT {
                self.offers.pop_front();
            }
        }
        HostMsg::FilesCopied { offer, files, folders }
    }

    /// Something other than files was copied. Returns the message that tells
    /// the client, if it thought files were there.
    pub fn withdraw(&mut self) -> Option<HostMsg> {
        if !std::mem::take(&mut self.offering) {
            return None;
        }
        self.last_offer += 1;
        Some(HostMsg::FilesCopied { offer: self.last_offer, files: vec![], folders: 0 })
    }

    pub fn open(&mut self, id: ChannelId) {
        self.fetches.insert(id, Fetch::Starting);
    }

    pub fn has(&self, id: ChannelId) -> bool {
        self.fetches.contains_key(&id)
    }

    /// Takes the client's request for a file. Returns the answer to send
    /// when the file can't be had.
    pub fn request(&mut self, id: ChannelId, data: &[u8]) -> Option<FileReply> {
        let fetch = self.fetches.get_mut(&id)?;
        if !matches!(fetch, Fetch::Starting) {
            return None;
        }
        let path = serde_json::from_slice::<FileRequest>(data).ok().and_then(|r| {
            let (_, paths) = self.offers.iter().find(|(offer, _)| *offer == r.offer)?;
            paths.get(r.index as usize).cloned()
        });
        let Some(path) = path else {
            *fetch = Fetch::Read;
            return Some(FileReply::Failed { message: "it isn't copied on this PC anymore".into() });
        };
        let (tx, rx) = mpsc::channel(16);
        std::thread::spawn(move || read(path, tx));
        *fetch = Fetch::Reading(rx);
        None
    }

    /// Everything read from this channel's file has been sent.
    pub fn read(&mut self, id: ChannelId) {
        if let Some(fetch) = self.fetches.get_mut(&id) {
            *fetch = Fetch::Read;
        }
    }

    /// The channel closed. Reading its file stops.
    pub fn close(&mut self, id: ChannelId) {
        self.fetches.remove(&id);
    }

    /// Checks how much file data is waiting on the connection. Call before
    /// [`Self::next`], whenever the connection may have sent some.
    pub fn measure(&mut self, rtc: &mut Rtc) {
        let mut queued = 0;
        for id in self.fetches.keys() {
            if let Some(mut channel) = rtc.channel(*id) {
                queued += channel.buffered_amount();
            }
        }
        self.room = queued < QUEUED_MAX;
    }

    /// The next message for a file channel, while there's room for it. None
    /// once the channel's file has all been read.
    pub async fn next(&mut self) -> (ChannelId, Option<Message>) {
        std::future::poll_fn(|cx| {
            if !self.room {
                return Poll::Pending;
            }
            for (id, fetch) in &mut self.fetches {
                if let Fetch::Reading(rx) = fetch {
                    if let Poll::Ready(message) = rx.poll_recv(cx) {
                        return Poll::Ready((*id, message));
                    }
                }
            }
            Poll::Pending
        })
        .await
    }
}

/// Splits what was copied into files the client can fetch and a count of
/// folders, which it can't. Looks as the signed-in user, who copied them.
fn describe(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<CopiedFile>, u32) {
    let user = User::signed_in().ok();
    let _as_user = user.as_ref().and_then(|u| u.impersonate().ok().flatten());
    let (mut kept, mut files, mut folders, mut bytes) = (vec![], vec![], 0, 0);
    for path in paths {
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => folders += 1,
            Ok(m) => {
                let name = file_name(&path);
                bytes += name.len() + 32;
                if bytes > OFFER_MAX_BYTES {
                    tracing::info!("too many files copied; offering the first {}", files.len());
                    break;
                }
                files.push(CopiedFile { name, size: m.len() });
                kept.push(path);
            }
            Err(e) => tracing::info!("can't offer {}: {e}", path.display()),
        }
    }
    (kept, files, folders)
}

/// Reads a copied file as the signed-in user and hands it over in messages
/// for its channel: a [`FileHeader`] and the bytes, or a [`FileReply::Failed`].
fn read(path: PathBuf, tx: mpsc::Sender<Message>) {
    let sent = || -> Result<bool> {
        let user = User::signed_in()?;
        let _as_user = user.impersonate()?;
        let mut file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let size = file.metadata().with_context(|| format!("reading {}", path.display()))?.len();
        let header = serde_json::to_vec(&FileHeader { name: file_name(&path), size })?;
        if tx.blocking_send((false, header)).is_err() {
            return Ok(false);
        }
        let mut left = size;
        while left > 0 {
            let mut chunk = vec![0; left.min(CHUNK as u64) as usize];
            file.read_exact(&mut chunk).with_context(|| format!("reading {}", path.display()))?;
            left -= chunk.len() as u64;
            // The client closed the channel.
            if tx.blocking_send((true, chunk)).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    };
    match sent() {
        Ok(true) => tracing::info!("sent {}", path.display()),
        Ok(false) => tracing::info!("stopped sending {}", path.display()),
        Err(e) => {
            tracing::warn!("sending a copied file: {e:#}");
            if let Ok(json) = serde_json::to_vec(&FileReply::Failed { message: format!("{e:#}") }) {
                _ = tx.blocking_send((false, json));
            }
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
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
/// The files on the clipboard as the signed-in user sees them, or `None` when
/// they copied something else. Windows hides copied files from SYSTEM, so the
/// agent asks a copy of itself running as that user. Run by hand, this
/// process already is the user and nothing is hidden.
pub fn copied_as_user() -> Result<Option<Vec<PathBuf>>> {
    let user = User::signed_in()?;
    let Some(token) = user.0 else { return Ok(None) };
    let exe = std::env::current_exe().context("finding conductor-host.exe")?;
    let out = crate::spawn::output_as(token, &exe, "copied-files", Duration::from_secs(5))?;
    serde_json::from_slice(&out).context("reading the copied files")
}

/// Prints the files on the clipboard as JSON, for [`copied_as_user`].
pub fn print_copied() -> Result<()> {
    let files = conductor_clipboard::read_files()?;
    println!("{}", serde_json::to_string(&files)?);
    Ok(())
}

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
