//! Text and copied files on the Windows clipboard, shared by the host and the app.

#[cfg(windows)]
mod windows_clipboard;

#[cfg(windows)]
pub use windows_clipboard::{read_files, read_text, sequence, write_text};
