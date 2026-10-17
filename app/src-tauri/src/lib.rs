//! Conductor app shell. The interface is the same web client every host
//! serves; this adds what a browser can't do on its own: read the tailnet
//! device list and switch hosting for this PC on or off.

#[cfg(windows)]
mod local;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(windows)]
    let builder =
        builder.invoke_handler(tauri::generate_handler![local::devices, local::host_status, local::set_hosting]);
    builder.run(tauri::generate_context!()).expect("error while running Conductor");
}
