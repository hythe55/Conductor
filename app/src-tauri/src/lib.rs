//! Conductor app shell. The interface is the same web client every host
//! serves; this adds what a browser can't do on its own: read the tailnet
//! device list, switch hosting for this PC on or off, save files from the
//! PC being controlled, and send it shortcuts like Alt+Tab. On a phone, it
//! finds the owner's PCs on the same Wi-Fi.

#[cfg(windows)]
mod downloads;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod local;
mod nearby;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Opening the app again shows the window that's already open.
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _, _| {
        use tauri::Manager;
        if let Some(window) = app.get_webview_window("main") {
            _ = window.unminimize();
            _ = window.show();
            _ = window.set_focus();
        }
    }));
    // How to wake the owner's PCs, remembered from when they were last online.
    #[cfg(windows)]
    let builder = builder.setup(|app| {
        use tauri::Manager;
        let path = app.path().app_local_data_dir()?.join("wake-peers.json");
        app.manage(conductor_tailnet::WakeMemory::open(path));
        Ok(())
    });
    #[cfg(windows)]
    let builder = builder.manage(downloads::Saving::default()).invoke_handler(tauri::generate_handler![
        local::devices,
        local::host_status,
        local::set_hosting,
        local::set_auto_update,
        local::set_previews,
        local::set_ask,
        local::set_nearby,
        local::check_for_updates,
        local::install_update,
        local::read_clipboard,
        local::write_clipboard,
        local::write_clipboard_image,
        local::clipboard_sequence,
        keys::capture_keys,
        keys::release_keys,
        downloads::begin_save,
        downloads::write_save,
        downloads::end_save,
        downloads::abort_save,
        nearby::nearby_pcs,
    ]);
    #[cfg(not(windows))]
    let builder = builder.invoke_handler(tauri::generate_handler![nearby::nearby_pcs]);
    builder.run(tauri::generate_context!()).expect("error while running Conductor");
}
