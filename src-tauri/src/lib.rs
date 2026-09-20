mod archive;
mod clustering;
mod commands;
mod comfy_finder;
mod db;
mod grouper;
mod metadata;
mod paths;
mod scoring;
mod workflow;

use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Log sink is decided BEFORE the app exists: in portable mode file logs
    // must follow the exe (data/logs), never the OS log dir.
    let log_builder = match paths::log_root_for_portable() {
        Some(dir) => tauri_plugin_log::Builder::default()
            .targets([tauri_plugin_log::Target::new(
                tauri_plugin_log::TargetKind::Folder {
                    path: dir,
                    file_name: None,
                },
            )]),
        None => tauri_plugin_log::Builder::default(),
    };

    let app = tauri::Builder::default()
        .plugin(log_builder.build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = paths::data_root(app.handle());
            let db_path: PathBuf = dir.join("ai-image-sorter.db");
            let db = db::open(&db_path)?;
            app.manage(commands::AppState {
                db: Arc::new(db),
            });

            #[cfg(desktop)]
            {
                // Always create the main window here (tauri.conf.json declares
                // none): the WebView2 profile must be routed into the portable
                // data root when a marker file is present next to the exe.
                let mut builder = tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::App("index.html".into()),
                )
                .title("AI Image Sorter")
                .inner_size(1280.0, 860.0)
                .min_inner_size(960.0, 640.0)
                .resizable(true);
                // NOTE: data_directory is deliberately NOT set. A user report
                // showed the packaged build failing to serve the embedded
                // frontend this way; isolation is instead achieved by wiping
                // the default WebView2 folder on portable exit (paths.rs).
                let _ = &mut builder;
                builder.build()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::find_comfy_sources,
            commands::find_workflow_templates,
            commands::refresh_workflow_templates,
            commands::sync_all,
            commands::list_dir_images,
            commands::add_source_and_scan,
            commands::list_sources,
            commands::set_model_deviation_config,
            commands::recluster_l1,
            commands::list_groups,
            commands::list_group_images,
            commands::list_labels,
            commands::upsert_label,
            commands::delete_label,
            commands::set_image_label,
            commands::swipe,
            commands::apply_swipe_action,
            commands::arena_vote,
            commands::arena_suggested,
            commands::list_group_images_all,
            commands::update_image_prompts,
            commands::toggle_hidden,
            commands::toggle_hidden_action,
            commands::undo_review_action,
            commands::trash_image,
            commands::recluster_source,
            commands::merge_groups,
            commands::split_images,
            commands::list_move_targets,
            commands::move_images_to_group,
            commands::undo_split,
            commands::unmerge_group,
            commands::arena_hide,
            commands::export_data,
            commands::archive_copy,
            commands::db_path,
            commands::get_group_thumbnails,
            commands::recommend_prompts,
            commands::record_telemetry_event,
            commands::export_diagnostics,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    let portable = paths::is_portable();
    app.run(move |app_handle, event| match event {
        tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
            if let Some(state) = app_handle.try_state::<commands::AppState>() {
                if let Err(e) = state.db.checkpoint() {
                    log::warn!("WAL checkpoint on shutdown failed: {e}");
                }
            }
            if portable {
                paths::clear_default_webview_data(app_handle);
            }
        }
        _ => {}
    });
}
