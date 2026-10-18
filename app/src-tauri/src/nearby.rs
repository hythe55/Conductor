//! Finds the owner's PCs on the phone's Wi-Fi, so the phone app can start
//! without being told the name of one.

use std::time::Duration;

use conductor_proto::nearby::{self, NearbyPc};

/// How long PCs get to answer.
const WAIT: Duration = Duration::from_millis(1500);

#[tauri::command]
pub async fn nearby_pcs() -> Result<Vec<NearbyPc>, String> {
    match tauri::async_runtime::spawn_blocking(|| nearby::find(WAIT)).await {
        Ok(found) => found.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}
