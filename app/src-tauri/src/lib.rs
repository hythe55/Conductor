//! Conductor app shell. The interface is the same web client every host
//! serves; this adds what a browser can't do on its own: read the tailnet
//! device list and switch hosting for this PC on or off.

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
    let builder = builder.invoke_handler(tauri::generate_handler![
        local::devices,
        local::host_status,
        local::set_hosting,
        local::set_auto_update,
        local::check_for_updates,
        local::install_update,
    ]);
    builder.run(tauri::generate_context!()).expect("error while running Conductor");
}
