//! Saves files copied on the PC being controlled in this PC's Downloads
//! folder, written as they arrive rather than held in memory.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use conductor_proto::files::create_unique;
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Manager, State};

use crate::local::Problem;

const COULDNT_SAVE: &str = "Couldn't save the file in this PC's Downloads folder. Check there's space for it.";

/// Files being saved, by the id `begin_save` handed out.
#[derive(Default)]
pub struct Saving {
    files: Mutex<HashMap<u32, (PathBuf, File)>>,
    last: AtomicU32,
}

/// Creates the file in Downloads, numbering the name when it's taken.
#[tauri::command]
pub async fn begin_save(app: AppHandle, saving: State<'_, Saving>, name: String) -> Result<u32, Problem> {
    let folder = app
        .path()
        .download_dir()
        .map_err(|e| Problem::new("Couldn't find this PC's Downloads folder.", e))?;
    let (path, file) = create_unique(&folder, &name).map_err(|e| Problem::new(COULDNT_SAVE, e))?;
    let id = saving.last.fetch_add(1, Ordering::Relaxed) + 1;
    saving.files.lock().unwrap().insert(id, (path, file));
    Ok(id)
}

/// Appends the raw request body to the file named by the `save-id` header.
#[tauri::command]
pub async fn write_save(saving: State<'_, Saving>, request: Request<'_>) -> Result<(), Problem> {
    let InvokeBody::Raw(data) = request.body() else {
        return Err(Problem::new(COULDNT_SAVE, "the file's bytes arrived as JSON"));
    };
    let id: u32 = request
        .headers()
        .get("save-id")
        .and_then(|v| v.to_str().ok()?.parse().ok())
        .ok_or_else(|| Problem::new(COULDNT_SAVE, "no save-id header"))?;
    let mut files = saving.files.lock().unwrap();
    let (path, file) = files.get_mut(&id).ok_or_else(|| Problem::new(COULDNT_SAVE, "the file isn't open"))?;
    file.write_all(data).map_err(|e| Problem::new(COULDNT_SAVE, format!("{}: {e}", path.display())))
}

/// Closes the finished file. Returns the name it was saved under.
#[tauri::command]
pub async fn end_save(saving: State<'_, Saving>, id: u32) -> Result<String, Problem> {
    let (path, file) =
        saving.files.lock().unwrap().remove(&id).ok_or_else(|| Problem::new(COULDNT_SAVE, "the file isn't open"))?;
    drop(file);
    Ok(path.file_name().unwrap_or_default().to_string_lossy().into_owned())
}

/// Deletes a file that didn't arrive in full.
#[tauri::command]
pub async fn abort_save(saving: State<'_, Saving>, id: u32) -> Result<(), Problem> {
    let Some((path, file)) = saving.files.lock().unwrap().remove(&id) else { return Ok(()) };
    drop(file);
    _ = std::fs::remove_file(path);
    Ok(())
}
