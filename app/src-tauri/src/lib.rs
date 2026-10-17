//! Conductor app shell. The interface is the same web client every host
//! serves; this adds what a browser can't do on its own: read the tailnet
//! device list, switch hosting for this PC on or off, and save files from
//! the PC being controlled.

#[cfg(windows)]
mod downloads;
#[cfg(windows)]
mod local;

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
    #[cfg(windows)]
    let builder = builder.manage(downloads::Saving::default()).invoke_handler(tauri::generate_handler![
        local::devices,
        local::host_status,
        local::set_hosting,
        local::set_auto_update,
        local::check_for_updates,
        local::install_update,
        local::read_clipboard,
        local::write_clipboard,
        downloads::begin_save,
        downloads::write_save,
        downloads::end_save,
        downloads::abort_save,
    ]);
    builder.run(tauri::generate_context!()).expect("error while running Conductor");
}
