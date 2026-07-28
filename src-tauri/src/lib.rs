mod archive;
mod clustering;
mod commands;
mod comfy_finder;
mod db;
mod grouper;
mod metadata;
mod scoring;
mod thumbnails;

use std::path::PathBuf;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .map_err(|e| anyhow::anyhow!("no app data dir: {e}"))?;
            std::fs::create_dir_all(&dir)?;
            let db_path = PathBuf::from(&dir).join("ai-image-sorter.db");
            let db = db::open(&db_path)?;
            app.manage(commands::AppState { db });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::find_comfy_sources,
            commands::list_dir_images,
            commands::add_source_and_scan,
            commands::list_sources,
            commands::list_groups,
            commands::list_group_images,
            commands::list_labels,
            commands::upsert_label,
            commands::delete_label,
            commands::set_image_label,
            commands::swipe,
            commands::arena_vote,
            commands::arena_suggested,
            commands::export_data,
            commands::archive_copy,
            commands::db_path,
            commands::get_group_thumbnails,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app_handle, event| match event {
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
            if let Some(state) = app_handle.try_state::<commands::AppState>() {
                if let Err(e) = state.db.checkpoint() {
                    log::warn!("WAL checkpoint on shutdown failed: {e}");
                }
            }
        }
        _ => {}
    });
}
