//! Saving files that came from another device, on the host and in the app.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// Creates a new file in `folder` for a file the other device called `name`.
/// A number is added to the name when a file of that name is already there.
pub fn create_unique(folder: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    let name = clean_name(name);
    let (stem, ext) = split_extension(&name);
    for n in 1..1000 {
        let path = folder.join(if n == 1 { name.clone() } else { format!("{stem} ({n}){ext}") });
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(ErrorKind::AlreadyExists, format!("too many files called {name}")))
}

/// A file name Windows accepts, made from the one the other device sent.
pub fn clean_name(name: &str) -> String {
    const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
    const MAX: usize = 150;
    // Only the name, even if it came with a folder.
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut s: String =
        name.chars().map(|c| if c < ' ' || r#"<>:"|?*"#.contains(c) { '_' } else { c }).collect();
    // Windows drops trailing dots and spaces itself, which could change the name.
    s.truncate(s.trim_end_matches(['.', ' ']).len());
    let s = s.trim_start();
    if s.is_empty() {
        return "File".into();
    }
    let device = s.split('.').next().unwrap_or_default().trim_end().to_ascii_uppercase();
    let is_device = RESERVED.contains(&device.as_str())
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && device.as_bytes()[3].is_ascii_digit());
    let mut s = if is_device { format!("_{s}") } else { s.to_string() };
    // Leave room for the folder within Windows' usual path limit.
    if s.chars().count() > MAX {
        let (stem, ext) = split_extension(&s);
        let ext = if ext.chars().count() <= 16 { ext } else { "" };
        let stem: String = stem.chars().take(MAX - ext.chars().count()).collect();
        s = format!("{}{ext}", stem.trim_end_matches(['.', ' ']));
    }
    s
}

/// `report.final.pdf` → (`report.final`, `.pdf`). Names starting with a dot have no extension.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(clean_name("report.pdf"), "report.pdf");
        assert_eq!(clean_name(r"C:\Users\x\report.pdf"), "report.pdf");
        assert_eq!(clean_name("../../evil.exe"), "evil.exe");
        assert_eq!(clean_name("a:b?.txt"), "a_b_.txt");
        assert_eq!(clean_name("name. . "), "name");
        assert_eq!(clean_name(".."), "File");
        assert_eq!(clean_name(""), "File");
        assert_eq!(clean_name("con.txt"), "_con.txt");
        assert_eq!(clean_name("COM1"), "_COM1");
        assert_eq!(clean_name("console.txt"), "console.txt");
        assert_eq!(clean_name(".gitignore"), ".gitignore");
        let long = format!("{}.pdf", "a".repeat(300));
        assert_eq!(clean_name(&long), format!("{}.pdf", "a".repeat(146)));
    }

    #[test]
    fn extensions() {
        assert_eq!(split_extension("a.tar.gz"), ("a.tar", ".gz"));
        assert_eq!(split_extension(".env"), (".env", ""));
        assert_eq!(split_extension("README"), ("README", ""));
    }

    #[test]
    fn numbered_when_taken() {
        let dir = std::env::temp_dir().join(format!("conductor-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, _) = create_unique(&dir, "x.txt").unwrap();
        let (b, _) = create_unique(&dir, "x.txt").unwrap();
        assert_eq!(a.file_name().unwrap(), "x.txt");
        assert_eq!(b.file_name().unwrap(), "x (2).txt");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
