//! Text, pictures and copied files on the Windows clipboard, shared by the
//! host and the app.

#[cfg(windows)]
mod image;
#[cfg(windows)]
mod windows_clipboard;

#[cfg(windows)]
pub use windows_clipboard::{formats, read_files, read_image, read_text, sequence, write_files, write_image, write_text};
