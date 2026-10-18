//! Files moving between the client and this PC, each on a data channel of
//! its own.
//!
//! Files the client sends are saved in the Downloads folder of whoever is
//! signed in to this PC. A JSON [`FileHeader`] comes first, then the bytes as
//! binary messages. Once the last byte is written the host answers with a
//! [`FileReply`] and the client closes the channel. A file whose channel
//! closes early is deleted. Files the client pastes arrive the same way but
//! wait in a folder of their own until they go on the clipboard.
//!
//! Files and pictures copied on this PC are offered to the client, which
//! fetches them one by one with a [`FileRequest`]. The host answers with a
//! [`FileHeader`] and the bytes, or a [`FileReply::Failed`].

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
use windows::Win32::UI::Shell::{FOLDERID_Downloads, FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::core::{GUID, HRESULT};

/// Pictures copied on either side bigger than this, as PNGs, aren't shared.
pub const IMAGE_MAX: usize = 32 << 20;
/// What a picture copied on this PC is called when the client saves it.
const IMAGE_NAME: &str = "Copied image.png";
/// Pasted files no longer on the clipboard are deleted after this long.
const PASTED_KEPT: Duration = Duration::from_secs(24 * 60 * 60);

enum Transfer {
    /// Waiting for the header.
    Starting,
    Receiving { path: PathBuf, file: BufWriter<File>, left: u64, paste: Option<u32> },
    /// Answered; waiting for the client to close the channel.
    Answered,
}

/// Files that arrived to be pasted, in a folder of their own.
pub struct Pasted {
    pub folder: PathBuf,
    files: Vec<PathBuf>,
}

/// The files arriving right now, by channel, and those waiting to be pasted.
#[derive(Default)]
pub struct Incoming {
    transfers: HashMap<ChannelId, Transfer>,
    pastes: HashMap<u32, Pasted>,
}

impl Incoming {
    pub fn open(&mut self, id: ChannelId) {
        self.transfers.insert(id, Transfer::Starting);
    }

    pub fn has(&self, id: ChannelId) -> bool {
        self.transfers.contains_key(&id)
    }

    /// Takes one message from a file channel. Returns the answer to send once
    /// the file is saved or has failed.
    pub fn receive(&mut self, id: ChannelId, binary: bool, data: &[u8]) -> Option<FileReply> {
        let transfer = self.transfers.get_mut(&id)?;
        match step(transfer, binary, data, &mut self.pastes) {
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
        if let Some(transfer) = self.transfers.remove(&id) {
            discard(transfer);
        }
    }

    /// The files that arrived for paste `id`, which are the caller's to
    /// paste from now on.
    pub fn take_paste(&mut self, id: u32) -> Option<Pasted> {
        self.pastes.remove(&id).filter(|p| !p.files.is_empty())
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        for (_, transfer) in self.transfers.drain() {
            discard(transfer);
        }
        // Never pasted.
        for (_, paste) in self.pastes.drain() {
            _ = std::fs::remove_dir_all(paste.folder);
        }
    }
}

/// Returns the saved file's name once its last byte is written.
fn step(
    transfer: &mut Transfer,
    binary: bool,
    data: &[u8],
    pastes: &mut HashMap<u32, Pasted>,
) -> Result<Option<String>> {
    match transfer {
        Transfer::Starting => {
            if binary {
                bail!("the file arrived without its name");
            }
            let header: FileHeader = serde_json::from_slice(data).context("reading the file's name and size")?;
            let (path, file) = match header.paste {
                Some(id) => {
                    let paste = match pastes.entry(id) {
                        Entry::Occupied(e) => e.into_mut(),
                        Entry::Vacant(e) => e.insert(Pasted { folder: paste_folder()?, files: vec![] }),
                    };
                    create_in(&paste.folder, &header.name)?
                }
                None => create(&header.name)?,
            };
            let file = BufWriter::with_capacity(1 << 20, file);
            *transfer = Transfer::Receiving { path, file, left: header.size, paste: header.paste };
        }
        Transfer::Receiving { path, file, left, .. } => {
            if !binary || data.len() as u64 > *left {
                bail!("more arrived than the file's size");
            }
            file.write_all(data).with_context(|| format!("writing {}", path.display()))?;
            *left -= data.len() as u64;
        }
        Transfer::Answered => return Ok(None),
    }
    match transfer {
        Transfer::Receiving { path, file, left: 0, paste } => {
            file.flush().with_context(|| format!("writing {}", path.display()))?;
            if let Some(p) = paste.and_then(|id| pastes.get_mut(&id)) {
                p.files.push(path.clone());
            }
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

/// Something copied on this PC that the client can fetch.
enum Offered {
    Files(Vec<PathBuf>),
    /// A picture, as a PNG.
    Image(Arc<Vec<u8>>),
}

/// Files copied on this PC, and the ones the client is fetching, by channel.
#[derive(Default)]
pub struct Outgoing {
    offers: VecDeque<(u32, Offered)>,
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
            self.keep(offer, Offered::Files(paths));
        }
        HostMsg::FilesCopied { offer, files, folders, image: false }
    }

    /// Makes a picture just copied on this PC fetchable and returns the
    /// message that tells the client about it.
    pub fn offer_image(&mut self, png: Vec<u8>) -> HostMsg {
        tracing::info!("copied a picture on this PC ({} KB)", png.len() / 1024);
        self.last_offer += 1;
        let offer = self.last_offer;
        self.offering = true;
        let files = vec![CopiedFile { name: IMAGE_NAME.into(), size: png.len() as u64 }];
        // Pictures can be big, so only the latest is kept.
        self.offers.retain(|(_, o)| !matches!(o, Offered::Image(_)));
        self.keep(offer, Offered::Image(Arc::new(png)));
        HostMsg::FilesCopied { offer, files, folders: 0, image: true }
    }

    fn keep(&mut self, offer: u32, offered: Offered) {
        self.offers.push_back((offer, offered));
        if self.offers.len() > OFFERS_KEPT {
            self.offers.pop_front();
        }
    }

    /// Something other than files was copied. Returns the message that tells
    /// the client, if it thought files were there.
    pub fn withdraw(&mut self) -> Option<HostMsg> {
        if !std::mem::take(&mut self.offering) {
            return None;
        }
        self.last_offer += 1;
        Some(HostMsg::FilesCopied { offer: self.last_offer, files: vec![], folders: 0, image: false })
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
        let found = serde_json::from_slice::<FileRequest>(data).ok().and_then(|r| {
            let (_, offered) = self.offers.iter().find(|(offer, _)| *offer == r.offer)?;
            match offered {
                Offered::Files(paths) => paths.get(r.index as usize).cloned().map(Ok),
                Offered::Image(png) => (r.index == 0).then(|| Err(png.clone())),
            }
        });
        let (tx, rx) = mpsc::channel(16);
        match found {
            Some(Ok(path)) => std::thread::spawn(move || read(path, tx)),
            Some(Err(png)) => std::thread::spawn(move || send_image(&png, tx)),
            None => {
                *fetch = Fetch::Read;
                return Some(FileReply::Failed { message: "it isn't copied on this PC anymore".into() });
            }
        };
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
        let header = serde_json::to_vec(&FileHeader { name: file_name(&path), size, paste: None })?;
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

/// Hands over a picture copied on this PC in messages for its channel.
fn send_image(png: &[u8], tx: mpsc::Sender<Message>) {
    let header = FileHeader { name: IMAGE_NAME.into(), size: png.len() as u64, paste: None };
    let Ok(header) = serde_json::to_vec(&header) else { return };
    if tx.blocking_send((false, header)).is_err() {
        return;
    }
    for chunk in png.chunks(CHUNK) {
        if tx.blocking_send((true, chunk.to_vec())).is_err() {
            tracing::info!("stopped sending a copied picture");
            return;
        }
    }
    tracing::info!("sent a copied picture");
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Creates a new file in the Downloads folder, as the signed-in user so it's
/// theirs and lands only where they could put it themselves.
fn create(name: &str) -> Result<(PathBuf, File)> {
    let user = User::signed_in()?;
    let folder = user.known_folder(&FOLDERID_Downloads, "Downloads")?;
    let _as_user = user.impersonate()?;
    create_unique(&folder, name).with_context(|| format!("saving {name} in {}", folder.display()))
}

/// Creates a new file in a folder of pasted files, as the signed-in user.
fn create_in(folder: &Path, name: &str) -> Result<(PathBuf, File)> {
    let user = User::signed_in()?;
    let _as_user = user.impersonate()?;
    create_unique(folder, name).with_context(|| format!("saving {name} in {}", folder.display()))
}

/// A new folder for one paste's files, in the signed-in user's app data, and
/// created as them. Folders from earlier pastes are deleted after a day.
fn paste_folder() -> Result<PathBuf> {
    let user = User::signed_in()?;
    let root = user.known_folder(&FOLDERID_LocalAppData, "app data")?.join("Conductor").join("Pasted");
    let _as_user = user.impersonate()?;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or_default() > PASTED_KEPT);
            if old {
                _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    for n in 0..100 {
        let folder = root.join(format!("{stamp}-{n}"));
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", folder.display())),
        }
    }
    bail!("couldn't create a folder for pasted files in {}", root.display())
}

/// Something copied that the agent can't read itself.
pub enum Hidden {
    Files(Vec<PathBuf>),
    /// A picture, as a PNG.
    Image(Vec<u8>),
}

/// What [`print_copied`] found, as JSON.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum Found {
    Files(Vec<PathBuf>),
    /// A PNG of the picture, saved in the user's temporary folder.
    Image(PathBuf),
}

/// The files or picture on the clipboard as the signed-in user sees them, or
/// `None` when they copied something else. Windows hides copied files from
/// SYSTEM, so the agent asks a copy of itself running as that user. Run by
/// hand, this process already is the user and has already looked for files.
pub fn copied_as_user() -> Result<Option<Hidden>> {
    let user = User::signed_in()?;
    let Some(token) = user.0 else {
        return Ok(conductor_clipboard::read_image(IMAGE_MAX)?.map(Hidden::Image));
    };
    let exe = std::env::current_exe().context("finding conductor-host.exe")?;
    let out = crate::spawn::output_as(token, &exe, "copied", Duration::from_secs(10))?;
    let found: Option<Found> = serde_json::from_slice(&out)
        .with_context(|| format!("reading what was copied: {}", String::from_utf8_lossy(&out).trim()))?;
    Ok(match found {
        None => None,
        Some(Found::Files(files)) => Some(Hidden::Files(files)),
        Some(Found::Image(path)) => {
            let png = std::fs::read(&path).with_context(|| format!("reading {}", path.display()));
            _ = std::fs::remove_file(&path);
            Some(Hidden::Image(png?))
        }
    })
}

/// Prints the files or picture on the clipboard as JSON, for [`copied_as_user`].
pub fn print_copied() -> Result<()> {
    let found = match conductor_clipboard::read_files()? {
        Some(files) => Some(Found::Files(files)),
        None => match conductor_clipboard::read_image(IMAGE_MAX)? {
            Some(png) => {
                let path = std::env::temp_dir().join(format!("conductor-copied-{}.png", std::process::id()));
                std::fs::write(&path, png).with_context(|| format!("saving {}", path.display()))?;
                Some(Found::Image(path))
            }
            None => None,
        },
    };
    println!("{}", serde_json::to_string(&found)?);
    Ok(())
}

/// Puts what arrived for a paste on the clipboard: the picture, or the files
/// as if copied in Explorer. As the signed-in user, like [`copied_as_user`],
/// so the apps they paste into see it as theirs.
pub fn paste(pasted: &Pasted, image: bool) -> Result<()> {
    let user = User::signed_in()?;
    let path = if image { &pasted.files[0] } else { &pasted.folder };
    let Some(token) = user.0 else { return put_on_clipboard(path, image) };
    let exe = std::env::current_exe().context("finding conductor-host.exe")?;
    let what = if image { "paste-image" } else { "paste-files" };
    let out = crate::spawn::output_as(token, &exe, &format!("{what} \"{}\"", path.display()), Duration::from_secs(10))?;
    let out = String::from_utf8_lossy(&out);
    if out.trim() != "pasted" {
        bail!("{}", out.trim());
    }
    Ok(())
}

/// Puts the picture at `path` on the clipboard, or the files in the folder
/// at `path`.
pub fn put_on_clipboard(path: &Path, image: bool) -> Result<()> {
    if image {
        let png = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        return conductor_clipboard::write_image(&png);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("reading {}", path.display()))?;
    files.sort();
    conductor_clipboard::write_files(&files)
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

    /// One of the user's folders, such as Downloads, called `what` in errors.
    fn known_folder(&self, id: &GUID, what: &str) -> Result<PathBuf> {
        unsafe {
            let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, self.0)
                .with_context(|| format!("finding the {what} folder"))?;
            let path = p.to_string();
            CoTaskMemFree(Some(p.0 as *const _));
            Ok(Path::new(&path.with_context(|| format!("finding the {what} folder"))?).to_path_buf())
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
