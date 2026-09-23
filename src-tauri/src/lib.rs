//! OneRust Tauri library — OneCLI firmware updater backend.

mod blueprint;
mod cancel;
mod catalog;
mod commands;
mod lenovo;
mod logutil;
mod update;

use cancel::JobCancel;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(JobCancel::new())
        .invoke_handler(tauri::generate_handler![
            commands::list_models,
            commands::resolve_machine_types,
            commands::download_bundles,
            commands::ensure_onecli_ready,
            commands::start_updates,
            commands::parse_hosts,
            commands::default_paths,
            commands::classify_blueprint,
            commands::apply_blueprint,
            commands::verify_blueprint,
            commands::cancel_jobs,
            commands::load_hosts_file,
            commands::open_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running OneRust");
}
