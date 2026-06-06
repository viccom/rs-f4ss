mod commands;
mod tray;

use std::sync::Arc;

use rs_f4ss_core::MountManager;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mgr = Arc::new(MountManager::new_with_persistence(
        rs_f4ss_core::persistence::default_config_path()
            .expect("Cannot determine config directory"),
    ));
    mgr.restore_entries();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .manage(mgr)
        .invoke_handler(tauri::generate_handler![
            commands::health,
            commands::version,
            commands::list_mounts,
            commands::get_mount,
            commands::create_mount,
            commands::update_mount,
            commands::delete_mount,
            commands::start_mount,
            commands::stop_mount,
            commands::restore_mounts,
        ])
        .setup(|app| {
            tray::setup_tray(app.handle())?;

            // Intercept window close → hide to tray instead of quitting
            if let Some(window) = app.get_webview_window("main") {
                let win = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
