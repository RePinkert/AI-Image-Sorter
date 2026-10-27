mod archive;
mod capabilities;
mod clustering;
mod commands;
mod comfy_finder;
mod db;
mod fingerprint;
mod grouper;
mod metadata;
mod paths;
mod progress;
mod reconcile;
mod scoring;
mod suggest;
mod text;
mod vision;
mod workflow;

use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 命令行入口：`--recover <旧库路径>` —— 从另一个库文件里把用户的手工
    // 分拣结果找回来（第 29 轮「库恢复」）。
    //
    // 为什么要有命令行这一条路：GUI 里也能做（设置页 → 库恢复），但**批量
    // 抢救**（用户的库被改名毁过、想一次性找回）用命令行更顺手，也能在
    // 应用没起来的时候跑。
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--recover") {
        let old_db = args.get(pos + 1).cloned().unwrap_or_default();
        if old_db.is_empty() {
            eprintln!("用法：AI-Image-Sorter.exe --recover <旧库文件路径>");
            std::process::exit(2);
        }
        std::process::exit(run_recover(&old_db));
    }
    run_gui();
}

/// 命令行版库恢复。返回进程退出码。
///
/// **报告写进文件**：发行版是 `windows_subsystem = "windows"`（不弹控制台），
/// 所以 stdout 用户看不见 —— 报告落到数据目录里，路径写进日志。
fn run_recover(old_db: &str) -> i32 {
    let dir = paths::data_root_for_cli();
    let db_path = dir.join("ai-image-sorter.db");
    let db = match db::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("打开当前库失败：{e}");
            return 2;
        }
    };
    let mut report: Vec<String> = Vec::new();
    fn say(report: &mut Vec<String>, line: String) {
        println!("{line}");
        report.push(line);
    }
    say(&mut report, format!("当前库：{}", db_path.display()));
    say(&mut report, format!("旧库：  {old_db}"));
    let preview = match reconcile::preview(&db, old_db) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("预览失败：{e}");
            return 2;
        }
    };
    say(&mut report, format!(
        "旧库共 {} 张，能按内容认领 {} 张（未认领 {} 张）",
        preview.old_images, preview.matched, preview.unmatched_old
    ));
    for (basis, count) in &preview.basis {
        say(&mut report, format!("  匹配依据 {basis}：{count} 张"));
    }
    say(&mut report, format!(
        "可找回：手动分组 {} 条、标签 {} 个、评分 {} 条（当前库已有而跳过 {} 条）",
        preview.bindings,
        preview.labels,
        preview.scores,
        preview.skipped_conflict
    ));
    let apply = std::env::args().any(|a| a == "--apply");
    if apply {
        match reconcile::apply(&db, old_db) {
            Ok(outcome) => say(&mut report, format!(
                "已找回：手动分组 {} 条、标签 {} 个、评分 {} 条（认领 {} 张，跳过 {} 条）",
                outcome.bindings,
                outcome.labels,
                outcome.scores,
                outcome.matched,
                outcome.skipped_conflict
            )),
            Err(e) => {
                say(&mut report, format!("恢复失败：{e}"));
                write_recover_report(&dir, &report);
                return 2;
            }
        }
    } else {
        say(&mut report, "尚未写入（这是预览）。加 --apply 才会真的写进当前库。".to_string());
    }
    let path = write_recover_report(&dir, &report);
    say(&mut report, format!("报告已写入 {}", path.display()));
    0
}

fn write_recover_report(dir: &PathBuf, lines: &[String]) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let path = dir.join(format!("recover-report-{stamp}.txt"));
    let body = lines.join("\n");
    // **UTF-8 with BOM**：这份报告是给人在 Windows 上用记事本 / PowerShell
    // 打开的，没有 BOM 的话中文会变乱码（与 .ps1 同一条纪律）。
    let mut bytes = vec![0xEFu8, 0xBBu8, 0xBFu8];
    bytes.extend_from_slice(body.as_bytes());
    let _ = std::fs::write(&path, &bytes);
    log::info!("库恢复报告：{} —— {}", path.display(), body.replace('\n', " / "));
    path
}

fn run_gui() {
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
            // 视觉签名引擎（阶段六）：模型权重缺失时功能不可用，但应用照常启动。
            let model_dir = vision::resolve_model_dir(app.handle());
            let model_path = model_dir
                .map(|d| d.join("vision_clip.onnx"))
                .unwrap_or_else(|| dir.join("models").join("vision_clip.onnx"));
            if !model_path.is_file() {
                log::info!("视觉模型权重缺失，识图功能不可用：{}", model_path.display());
            }
            // 文本塔（阶段七轮 1 接线）：权重与分词器**缺任一份**就不可用，
            // 但应用照常启动 —— 与视觉塔同口径。
            let text_model_dir = text::resolve_text_model_dir(app.handle());
            let (text_model_path, text_tokenizer_path) = match text_model_dir {
                Some(d) => (d.join(text::TEXT_WEIGHTS), d.join(text::TEXT_TOKENIZER)),
                None => {
                    let fallback = dir.join("models");
                    (
                        fallback.join(text::TEXT_WEIGHTS),
                        fallback.join(text::TEXT_TOKENIZER),
                    )
                }
            };
            if !text_model_path.is_file() || !text_tokenizer_path.is_file() {
                log::info!(
                    "文本塔资源缺失，prompt 语义功能不可用：{} / {}",
                    text_model_path.display(),
                    text_tokenizer_path.display()
                );
            }
            app.manage(commands::AppState {
                db: Arc::new(db),
                vision: Arc::new(vision::VisionEngine::new(model_path)),
                text: Arc::new(text::TextEngineSlot::new(
                    text_model_path,
                    text_tokenizer_path,
                )),
                signature_cancel: std::sync::Mutex::new(None),
                text_signature_cancel: std::sync::Mutex::new(None),
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
            commands::hide_group_images,
            commands::trash_group_images,
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
            commands::vision_status,
            commands::compute_signatures,
            commands::cancel_signatures,
            commands::text_status,
            commands::text_coverage,
            commands::l2_signature_coverage,
            commands::compute_text_signatures,
            commands::cancel_text_signatures,
            commands::suggest_move_targets,
            commands::group_outliers,
            commands::app_capabilities,
            commands::list_library_candidates,
            commands::preview_library_recovery,
            commands::apply_library_recovery,
            commands::discard_unconfirmed_image,
            commands::list_unconfirmed_images,
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
