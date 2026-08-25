use crate::db::{ImageInsert, ImageRow, LabelRow, SourceRow};
use crate::grouper::compute_group_keys;
use crate::metadata::{parse_file, ImageMeta};
use crate::{archive, comfy_finder, db::GroupInfo, scoring};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager, State};

pub struct AppState {
    pub db: Arc<crate::db::Db>,
}

/// Files modified within this window are assumed to be mid-write and are
/// deferred to the next sync cycle instead of being parsed into broken rows.
const STABILITY_WINDOW_SECS: i64 = 10;

/// How often the 8s poll may re-walk the ComfyUI workflow template
/// directories. Template files rarely change, so a full re-read every poll
/// is wasted I/O; we refresh on change OR at most this often.
const TEMPLATE_REFRESH_INTERVAL_SECS: u64 = 60;

/// Process-wide throttle timestamp for `refresh_templates`.
static LAST_TEMPLATE_REFRESH: Mutex<Option<SystemTime>> = Mutex::new(None);

#[derive(Debug, Clone, Serialize)]
pub struct SyncProgressDto {
    pub stage: String,
    pub source_index: usize,
    pub source_total: usize,
    pub source_path: String,
    pub found: usize,
    pub processed: usize,
    pub added: usize,
    pub pending: usize,
    pub parse_errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncAllResult {
    pub sources: usize,
    pub added: usize,
    pub pending: usize,
    pub reclustered: bool,
    pub parse_errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundSourceDto {
    pub path: String,
    pub kind: String,
    pub origin: String,
}

#[tauri::command]
pub fn find_comfy_sources() -> Vec<FoundSourceDto> {
    comfy_finder::find_comfy_outputs()
        .into_iter()
        .map(|s| FoundSourceDto {
            path: s.path.to_string_lossy().to_string(),
            kind: s.kind.to_string(),
            origin: s.origin,
        })
        .collect()
}

#[tauri::command]
pub fn find_workflow_templates(state: State<'_, AppState>) -> Result<Vec<crate::workflow::WorkflowTemplateDto>, String> {
    let sources = state.db.list_sources().map_err(|e| e.to_string())?;
    let mut templates = Vec::new();
    for source in sources {
        templates.extend(crate::workflow::find_templates_from_output(
            &crate::workflow::output_path_from_source(&source.path),
        ));
    }
    templates.sort_by(|a, b| a.path.cmp(&b.path));
    templates.dedup_by(|a, b| a.path == b.path);
    Ok(templates)
}

#[tauri::command]
pub fn list_dir_images(path: String) -> Vec<String> {
    comfy_finder::list_images_in(&PathBuf::from(&path))
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddSourceArgs {
    pub path: String,
    pub kind: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub source_id: i64,
    pub scanned: usize,
    pub groups: usize,
    pub parse_errors: usize,
}

/// Canonicalize a path for stable cross-rescan matching so that the same
/// file on disk always resolves to the same existing row — regardless of
/// how the user re-picked the source folder (trailing slash, case variants
/// on case-insensitive filesystems, forward/back slashes, .. segments).
/// We deliberately do NOT call `fs::canonicalize` (which resolves symlinks
/// and mandates existence); we only normalize the textual form.
pub fn normalize_path_str(s: &str) -> String {
    normalize_inner(s)
}

fn normalize_path(p: &std::path::Path) -> String {
    normalize_inner(&p.to_string_lossy())
}

fn normalize_inner(input: &str) -> String {
    let mut s = input.to_string();
    // Strip Windows NT namespace prefix (\\?\ or \\.\) which can appear
    // in paths returned by file dialogs or COM API on Windows.
    if s.starts_with("\\\\?\\") || s.starts_with("\\\\.\\") {
        s = s[4..].to_string();
    } else if s.starts_with("//?/") || s.starts_with("//./") {
        s = s[4..].to_string();
    }
    // Normalize separators to '/' for storage; we restore OS form when
    // comparing to on-disk walk results (which we also normalize).
    if std::path::MAIN_SEPARATOR == '\\' {
        s = s.replace('\\', "/");
    }
    // Strip trailing separators (but keep the root "/" on *nix).
    while s.ends_with('/') && s.len() > 1 {
        s.pop();
    }
    // Lowercase the drive letter on Windows so C:/... and c:/... match.
    #[cfg(windows)]
    {
        if s.len() >= 2 {
            let bytes = s.as_bytes();
            if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                let mut arr = s.into_bytes();
                arr[0] = arr[0].to_ascii_lowercase();
                s = String::from_utf8(arr).unwrap_or_else(|_| String::new());
            }
        }
    }
    s
}

#[tauri::command]
pub async fn add_source_and_scan(
    args: AddSourceArgs,
    state: State<'_, AppState>,
) -> Result<ScanResult, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Normalize the source path itself so re-adding the same folder via a
        // slightly different string form upserts rather than creating a new
        // source row.
        let norm_source = normalize_path(&PathBuf::from(&args.path));
        let source_id = db
            .upsert_source(&norm_source, &args.kind, args.alias.as_deref())
            .map_err(|e| e.to_string())?;

        let (files, _) = comfy_finder::list_images_with_newest(&PathBuf::from(&norm_source));
        let stats = scan_source(&db, source_id, &norm_source, &files)?;
        db.set_source_stats(
            source_id,
            (stats.kept + stats.added) as i64,
            stats.newest_mtime,
        )
        .map_err(|e| e.to_string())?;
        let backfilled = backfill_source(&db, source_id, &norm_source)?;
        let mut affected = stats.affected.clone();
        affected.extend(backfilled.0);
        if !affected.is_empty() {
            let threshold = db.get_source_threshold(source_id).unwrap_or(crate::clustering::DEFAULT_L2_THRESHOLD);
            crate::clustering::recluster_l2_keys(&db, source_id, &affected, threshold)
                .map_err(|e| e.to_string())?;
        }
        Ok(ScanResult {
            source_id,
            scanned: stats.found,
            groups: stats.affected.len(),
            parse_errors: stats.parse_errors + backfilled.1,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Debug, Clone, Default)]
struct ScanStats {
    found: usize,
    added: usize,
    kept: usize,
    pending: usize,
    parse_errors: usize,
    /// Max mtime over PROCESSED files only (kept + added). Pending files
    /// are deliberately excluded so a deferred file keeps the source
    /// "changed" until a later cycle actually parses it — otherwise the
    /// change-detection watermark would match the disk and the pending
    /// file would never be scanned (the "1 image not loaded" forever bug).
    newest_mtime: i64,
    affected: HashSet<String>,
}

/// Idempotent scan of one source folder: keep rows for files still on disk
/// (their id and thus their scores/labels survive), insert brand-new files
/// only, and delete rows whose abs_path no longer exists (cascades to
/// score/label). Files modified in the last `STABILITY_WINDOW_SECS` seconds
/// are deferred so partially-written files never become permanently broken
/// rows. Match on NORMALIZED paths so trailing slashes / case / separator
/// differences can never cause a file to be mis-detected as "new" → which
/// would re-allocate image_id and CASCADE-delete its scores.
///
/// `files` is the caller's one-pass walk result (paths already sorted), so
/// the changed-source sync path doesn't walk the tree a second time.
fn scan_source(
    db: &crate::db::Db,
    source_id: i64,
    norm_source: &str,
    files: &[PathBuf],
) -> Result<ScanStats, String> {
    let existing = db
        .list_source_image_paths(source_id)
        .map_err(|e| e.to_string())?;
    let mut existing_map: HashMap<String, i64> = HashMap::new();
    for (id, p) in &existing {
        existing_map.insert(normalize_path(&PathBuf::from(p)), *id);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut stats = ScanStats {
        found: files.len(),
        ..Default::default()
    };

    let mut on_disk: HashSet<String> = HashSet::new();
    for f in files {
        let norm = normalize_path(f);
        on_disk.insert(norm.clone());
        // Track the newest mtime among files that are actually kept in the
        // DB this cycle (kept + added). Deferred files are excluded so the
        // watermark never "advances past" an unparsed file.
        let file_mtime = std::fs::metadata(f)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        if existing_map.contains_key(&norm) {
            // Keep the stable image id while refreshing file-level fields that
            // can change outside the app.
            let size = std::fs::metadata(f).map(|m| m.len() as i64).unwrap_or(0);
            let modified_at = file_mtime.unwrap_or(0);
            if let Some(id) = existing_map.get(&norm) {
                db.update_file_stats(*id, size, modified_at)
                    .map_err(|e| e.to_string())?;
            }
            stats.kept += 1;
            if let Some(m) = file_mtime {
                if m > stats.newest_mtime {
                    stats.newest_mtime = m;
                }
            }
            continue;
        }
        // File-stability check: defer files that are likely still being
        // written by ComfyUI (mtime in the last 10 seconds). They will be
        // picked up on the next cycle.
        if let Some(m) = file_mtime {
            if now.saturating_sub(m) < STABILITY_WINDOW_SECS {
                stats.pending += 1;
                continue;
            }
        }

        let meta = match parse_file(f) {
            Ok(meta) => meta,
            Err(error) => {
                stats.parse_errors += 1;
                log::warn!("image metadata parse failed for source {source_id}: {error}");
                ImageMeta::default()
            }
        };
        let keys = compute_group_keys(&meta, norm_source);
        stats.affected.insert(keys.l1.clone());
        let size = std::fs::metadata(f).map(|m| m.len() as i64).unwrap_or(0);
        if let Some(m) = file_mtime {
            if m > stats.newest_mtime {
                stats.newest_mtime = m;
            }
        }
        let seed = meta.samplers.first().map(|s| s.seed).unwrap_or(0);
        let steps = meta.samplers.first().map(|s| s.steps).unwrap_or(0);
        let cfg = meta.samplers.first().map(|s| s.cfg).unwrap_or(0.0);
        // Store the NORMALIZED abs_path so future rescans match this row by
        // the same normalization function, regardless of how the user
        // re-picked the source folder.
        let abs = norm.clone();

        let row = ImageInsert {
            source_id,
            rel_path: abs.clone(),
            abs_path: abs,
            filename: f
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            size,
            modified_at: file_mtime.unwrap_or(0),
            width: meta.width as i64,
            height: meta.height as i64,
            prompt_pos: meta.prompt_pos.clone(),
            prompt_neg: meta.prompt_neg.clone(),
            checkpoint: meta.checkpoint.clone(),
            loras: serde_json::to_string(&meta.loras).unwrap_or_default(),
            vae: meta.vae.clone(),
            samplers: serde_json::to_string(&meta.samplers).unwrap_or_default(),
            seed,
            steps,
            cfg,
            group_key_l0: keys.l0,
            group_key_l1: keys.l1,
            group_key_l2: keys.l2,
            group_key_l3: keys.l3,
            // SHA-256 was previously computed for every new file (full read
            // of multi-MB PNGs) but is never read anywhere; the column is
            // kept for future content-level dedup but left empty.
            sha256: String::new(),
            meta_ok: meta.raw_ok,
            source_kind: format!("{:?}", meta.source_kind).to_lowercase(),
            diffusion_model: meta.diffusion_model.clone(),
            model_chain_json: serde_json::to_string(&meta.model_chain)
                .unwrap_or_else(|_| "[]".to_string()),
            workflow_key: meta.workflow_key.clone(),
            workflow_graph_json: meta.workflow_graph_json.clone(),
            workflow_template_id: None,
            workflow_match_confidence: None,
            generation_recipe_json: meta.generation_recipe_json(),
            recipe_signature: meta.recipe_signature(),
            parser_version: crate::metadata::PARSER_VERSION.to_string(),
        };
        db.insert_image(&row).map_err(|e| e.to_string())?;
        stats.added += 1;
    }

    // Remove rows for files that vanished from disk.
    let (removed, deleted_l1) = db
        .delete_missing_source_images(source_id, &on_disk)
        .map_err(|e| e.to_string())?;
    stats.affected.extend(deleted_l1);
    let _ = removed;
    Ok(stats)
}

/// Re-parse rows whose metadata is missing or outdated (mid-write failures
/// before the stability check, pre-lenient-parser rows, pre-v8 rows without
/// workflow identity). Preserves image id / scores / labels. Returns the
/// L1 keys that changed so the caller can re-cluster them.
fn backfill_source(
    db: &crate::db::Db,
    source_id: i64,
    norm_source: &str,
) -> Result<(HashSet<String>, usize), String> {
    let rows = db
        .list_images_needing_reparse(source_id)
        .map_err(|e| e.to_string())?;
    let mut affected: HashSet<String> = HashSet::new();
    let mut parse_errors = 0usize;
    for (id, abs) in rows {
        let path = PathBuf::from(&abs);
        if !path.exists() {
            continue;
        }
        let meta = match parse_file(&path) {
            Ok(meta) => meta,
            Err(error) => {
                parse_errors += 1;
                log::warn!("image metadata backfill failed for source {source_id}: {error}");
                ImageMeta::default()
            }
        };
        // Skip files that still yield nothing (e.g. still mid-write); the
        // next sync retries them. No churn for genuinely bare files.
        if !meta.raw_ok && meta.workflow_key.is_empty() && meta.prompt_pos.is_empty() {
            continue;
        }
        let keys = compute_group_keys(&meta, norm_source);
        let size = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
        let seed = meta.samplers.first().map(|s| s.seed).unwrap_or(0);
        let steps = meta.samplers.first().map(|s| s.steps).unwrap_or(0);
        let cfg = meta.samplers.first().map(|s| s.cfg).unwrap_or(0.0);
        let row = ImageInsert {
            source_id,
            rel_path: abs.clone(),
            abs_path: abs.clone(),
            filename: path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            size,
            modified_at: std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            width: meta.width as i64,
            height: meta.height as i64,
            prompt_pos: meta.prompt_pos.clone(),
            prompt_neg: meta.prompt_neg.clone(),
            checkpoint: meta.checkpoint.clone(),
            loras: serde_json::to_string(&meta.loras).unwrap_or_default(),
            vae: meta.vae.clone(),
            samplers: serde_json::to_string(&meta.samplers).unwrap_or_default(),
            seed,
            steps,
            cfg,
            group_key_l0: keys.l0,
            group_key_l1: keys.l1.clone(),
            group_key_l2: keys.l2,
            group_key_l3: keys.l3,
            sha256: String::new(),
            meta_ok: meta.raw_ok,
            source_kind: format!("{:?}", meta.source_kind).to_lowercase(),
            diffusion_model: meta.diffusion_model.clone(),
            model_chain_json: serde_json::to_string(&meta.model_chain)
                .unwrap_or_else(|_| "[]".to_string()),
            workflow_key: meta.workflow_key.clone(),
            workflow_graph_json: meta.workflow_graph_json.clone(),
            workflow_template_id: None,
            workflow_match_confidence: None,
            generation_recipe_json: meta.generation_recipe_json(),
            recipe_signature: meta.recipe_signature(),
            parser_version: crate::metadata::PARSER_VERSION.to_string(),
        };
        let new_l1 = db
            .update_reparsed_image(id, &row, source_id)
            .map_err(|e| e.to_string())?;
        affected.insert(new_l1);
    }
    Ok((affected, parse_errors))
}

/// Rescan the ComfyUI workflow template directories of every registered
/// source and upsert the discovered templates into the DB.
fn refresh_templates(db: &crate::db::Db) -> Result<usize, String> {
    let sources = db.list_sources().map_err(|e| e.to_string())?;
    let mut keep: HashSet<String> = HashSet::new();
    let mut count = 0usize;
    for source in sources {
        for tpl in crate::workflow::find_templates_from_output(
            &crate::workflow::output_path_from_source(&source.path),
        ) {
            keep.insert(tpl.path.clone());
            db.upsert_workflow_template(
                &tpl.name,
                &tpl.path,
                tpl.workflow_id.as_deref(),
                &tpl.topology_signature,
                &tpl.graph_json,
                tpl.node_count,
                &tpl.diffusion_models,
                &tpl.model_chain,
            )
            .map_err(|e| e.to_string())?;
            count += 1;
        }
    }
    let _ = db.prune_workflow_templates(&keep).map_err(|e| e.to_string())?;
    Ok(count)
}

/// One-pass template assignment: for every distinct workflow key without a
/// template yet, match against the saved templates (exact signature or
/// valid subgraph) and write the match back for all its images. Early-outs
/// when nothing is unmatched so the 8s poll doesn't reload every template
/// row just to find nothing to do.
fn match_templates(db: &crate::db::Db) -> Result<usize, String> {
    let keys = db
        .distinct_unmatched_workflow_keys()
        .map_err(|e| e.to_string())?;
    if keys.is_empty() {
        return Ok(0);
    }
    let templates = db.list_workflow_templates().map_err(|e| e.to_string())?;
    let mut assigned = 0usize;
    for (key, graph_json) in keys {
        if let Some((tpl_id, confidence)) =
            crate::workflow::match_template(&graph_json, &templates)
        {
            assigned += db
                .assign_workflow_template(&key, tpl_id, confidence)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(assigned)
}

/// Background sync of all registered sources. Runs off the main thread and
/// emits `sync-progress` events so the UI can show live counts and never
/// freezes. Skips sources whose on-disk file count / newest mtime are
/// unchanged since the last scan, and only re-clusters the workflow groups
/// that actually gained or lost images.
#[tauri::command]
pub async fn sync_all(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<SyncAllResult, String> {
    let db = state.db.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_sync(&db, &handle))
        .await
        .map_err(|e| e.to_string())?
}

fn run_sync(
    db: &crate::db::Db,
    app: &tauri::AppHandle,
) -> Result<SyncAllResult, String> {
    let sources = db.list_sources().map_err(|e| e.to_string())?;
    let source_total = sources.len();
    let mut total_added = 0usize;
    let mut total_pending = 0usize;
    let mut total_parse_errors = 0usize;
    let mut any_recluster = false;
    let mut any_changed = false;

    for (index, src) in sources.iter().enumerate() {
        let emit = |stage: &str, found: usize, processed: usize, added: usize, pending: usize, parse_errors: usize| {
            let _ = app.emit(
                "sync-progress",
                SyncProgressDto {
                    stage: stage.to_string(),
                    source_index: index,
                    source_total,
                    source_path: src.path.clone(),
                    found,
                    processed,
                    added,
                    pending,
                    parse_errors,
                },
            );
        };

        let norm_source = normalize_path(&PathBuf::from(&src.path));
        let (files, newest) = comfy_finder::list_images_with_newest(&PathBuf::from(&src.path));
        let (stored_count, stored_newest) = db
            .get_source_stats(src.id)
            .map_err(|e| e.to_string())?;
        // Re-cluster affected groups at the threshold the USER last applied
        // for this source, not a hardcoded default — otherwise every new
        // image generation would split the groups they merged at a lower
        // threshold ("越切越碎").
        let l2_threshold = db
            .get_source_threshold(src.id)
            .map_err(|e| e.to_string())?;

        // Only touch sources whose on-disk state actually changed since the
        // last scan. Unchanged sources emit NO progress events so the UI
        // sync bar stays silent on no-op polls.
        let changed = stored_count != files.len() as i64 || stored_newest != newest;
        any_changed |= changed;
        let mut affected: HashSet<String> = HashSet::new();
        let mut found = files.len();
        let mut processed = 0usize;
        let mut added = 0usize;
        let mut pending = 0usize;

        if changed {
            let stats = scan_source(db, src.id, &norm_source, &files)?;
            // Store the PROCESSED watermark (kept + added), NOT the raw
            // disk count/newest — a stability-window-deferred file must keep
            // the source "changed" so a later cycle actually scans it.
            db.set_source_stats(
                src.id,
                (stats.kept + stats.added) as i64,
                stats.newest_mtime,
            )
            .map_err(|e| e.to_string())?;
            affected.extend(stats.affected);
            found = stats.found;
            processed = stats.kept + stats.added;
            added = stats.added;
            pending = stats.pending;
            total_parse_errors += stats.parse_errors;
            total_added += stats.added;
            total_pending += stats.pending;
            emit("scan", found, processed, added, pending, stats.parse_errors);
        }

        let (backfilled, backfill_parse_errors) = backfill_source(db, src.id, &norm_source)?;
        let backfilled_count = backfilled.len();
        total_parse_errors += backfill_parse_errors;
        affected.extend(backfilled);

        if !affected.is_empty() {
            emit(
                "recluster",
                found,
                processed + backfilled_count,
                added,
                pending,
                backfill_parse_errors,
            );
            crate::clustering::recluster_l2_keys(
                db,
                src.id,
                &affected,
                l2_threshold,
            )
            .map_err(|e| e.to_string())?;
            any_recluster = true;
        }
        if changed || backfilled_count > 0 {
            emit(
                "scan-done",
                found,
                processed + backfilled_count,
                added,
                pending,
                backfill_parse_errors,
            );
        }
    }

    // Refresh the saved-workflow template table at most once per interval,
    // OR whenever a source actually changed (editing a workflow usually
    // coincides with generating images). Previously this walked + re-read
    // every ComfyUI workflow JSON on every 8s poll even when nothing moved.
    let template_due = {
        let mut last = LAST_TEMPLATE_REFRESH.lock().unwrap();
        let due = last
            .map(|t| {
                t.elapsed()
                    .map(|e| e.as_secs() >= TEMPLATE_REFRESH_INTERVAL_SECS)
                    .unwrap_or(false)
            })
            .unwrap_or(true);
        if due || any_changed {
            *last = Some(std::time::SystemTime::now());
            true
        } else {
            false
        }
    };
    if template_due {
        let _ = refresh_templates(db);
    }

    let assigned = match_templates(db).unwrap_or(0);
    let _ = assigned;
    Ok(SyncAllResult {
        sources: source_total,
        added: total_added,
        pending: total_pending,
        reclustered: any_recluster,
        parse_errors: total_parse_errors,
    })
}

#[tauri::command]
pub fn refresh_workflow_templates(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    refresh_templates(&state.db)
}

#[tauri::command]
pub fn list_sources(state: State<'_, AppState>) -> Result<Vec<SourceRow>, String> {
    state.db.list_sources().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelDeviationConfig {
    pub source_id: i64,
    pub dimensions: Vec<String>,
}

#[tauri::command]
pub fn set_model_deviation_config(
    config: ModelDeviationConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let allowed = ["base_model", "checkpoint", "lora", "workflow"];
    if config.dimensions.iter().any(|d| !allowed.contains(&d.as_str())) {
        return Err("包含不支持的 Model偏差维度".into());
    }
    state.db.set_model_deviation_dimensions(config.source_id, &config.dimensions)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_groups(
    source_id: Option<i64>,
    level: u8,
    state: State<'_, AppState>,
) -> Result<Vec<GroupInfo>, String> {
    state.db.list_groups(source_id, level).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_group_images(
    group_key: String,
    level: u8,
    state: State<'_, AppState>,
) -> Result<Vec<ImageRow>, String> {
    state
        .db
        .list_group_images(&group_key, level)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_labels(state: State<'_, AppState>) -> Result<Vec<LabelRow>, String> {
    state.db.list_labels().map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct LabelInput {
    pub id: Option<i64>,
    pub name: String,
    pub gesture: String,
    pub color: Option<String>,
}

#[tauri::command]
pub fn upsert_label(
    input: LabelInput,
    state: State<'_, AppState>,
) -> Result<i64, String> {
    state
        .db
        .upsert_label(input.id, &input.name, &input.gesture, input.color.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_label(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    state.db.delete_label(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_image_label(
    image_id: i64,
    label_id: i64,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .db
        .set_image_label(image_id, label_id, on)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn swipe(
    image_id: i64,
    gesture: String,
    state: State<'_, AppState>,
) -> Result<f64, String> {
    state
        .db
        .apply_swipe_action(image_id, &gesture, None, None, None, None)
        .map(|result| result.score)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn apply_swipe_action(
    image_id: i64,
    gesture: String,
    label_id: Option<i64>,
    session_id: Option<String>,
    started_at: Option<String>,
    context_signature: Option<String>,
    state: State<'_, AppState>,
) -> Result<crate::db::ActionResult, String> {
    state
        .db
        .apply_swipe_action(
            image_id,
            &gesture,
            label_id,
            session_id.as_deref(),
            started_at.as_deref(),
            context_signature.as_deref(),
        )
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArenaArgs {
    pub group_key: String,
    pub left: i64,
    pub right: i64,
    pub winner_is_left: bool,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub context_signature: Option<String>,
}

#[tauri::command]
pub fn arena_vote(args: ArenaArgs, state: State<'_, AppState>) -> Result<(f64, f64), String> {
    state
        .db
        .arena_vote_atomic(
            &args.group_key,
            args.left,
            args.right,
            args.winner_is_left,
            args.session_id.as_deref(),
            args.started_at.as_deref(),
            args.context_signature.as_deref(),
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn arena_suggested(left: i64, right: i64, state: State<'_, AppState>) -> Result<bool, String> {
    let conn = state.db.0.lock().unwrap();
    let l: f64 = conn
        .query_row(
            "SELECT internal_score FROM scores WHERE image_id=?1",
            rusqlite::params![left],
            |r| r.get(0),
        )
        .unwrap_or(scoring::BASE_SCORE);
    let r: f64 = conn
        .query_row(
            "SELECT internal_score FROM scores WHERE image_id=?1",
            rusqlite::params![right],
            |r| r.get(0),
        )
        .unwrap_or(scoring::BASE_SCORE);
    drop(conn);
    Ok(scoring::arena_suggested(l, r))
}

#[tauri::command]
pub fn list_group_images_all(
    group_key: String,
    level: u8,
    state: State<'_, AppState>,
) -> Result<Vec<ImageRow>, String> {
    state
        .db
        .list_group_images_all(&group_key, level)
        .map_err(|e| e.to_string())
}

/// Toggle an image's hidden flag. Hidden images are excluded from swipe /
/// arena scoring but still surfaced (with a gray overlay) in FolderView so
/// the user can un-block them. Use case: gross generation artifacts (extra
/// hands, etc.) that the user wants to never see in the deck but keep the
/// file itself on disk.
///
/// Hiding also pins the image's score to 0 so it can never win a swipe /
/// arena round or float to the top of score-sorted views. Un-hiding does
/// NOT restore the previous score (the user can re-score via swipe/arena).
#[tauri::command]
pub fn toggle_hidden(
    image_id: i64,
    hidden: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .db
        .toggle_hidden_atomic(image_id, hidden, None, None, None)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn toggle_hidden_action(
    image_id: i64,
    hidden: bool,
    session_id: Option<String>,
    started_at: Option<String>,
    context_signature: Option<String>,
    state: State<'_, AppState>,
) -> Result<crate::db::ActionResult, String> {
    state
        .db
        .toggle_hidden_atomic(
            image_id,
            hidden,
            session_id.as_deref(),
            started_at.as_deref(),
            context_signature.as_deref(),
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn undo_review_action(
    action_id: String,
    session_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<crate::db::UndoActionResult, String> {
    state
        .db
        .undo_review_action(&action_id, session_id.as_deref())
        .map_err(|e| e.to_string())
}

/// Send an image file to the operating-system recycle bin (Windows:
/// SHFileOperation with FOF_ALLOWUNDO — fully reversible from the desktop
/// Recycle Bin) and remove its DB row. Scores/labels cascade-delete with
/// the image_id FK, which matches user intent ("this picture is gone, get
/// rid of its metadata too"). The file is *not* permanently deleted; the
/// user can restore it from the Recycle Bin if they change their mind.
#[tauri::command]
pub fn trash_image(
    image_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let abs = state
        .db
        .image_abs_path(image_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "image row not found".to_string())?;
    let native = if std::path::MAIN_SEPARATOR == '\\' {
        abs.replace('/', "\\")
    } else {
        abs
    };
    let p = PathBuf::from(&native);
    if p.exists() {
        trash::delete(&p).map_err(|e| e.to_string())?;
    }
    state
        .db
        .delete_image_by_id(image_id)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Re-cluster L2 within a source at an explicit threshold (driven by the
/// Settings slider). Does NOT trigger a re-scan — purely recomputes the
/// `group_key_l2` column from existing prompt_pos values using the supplied
/// Jaccard threshold. Caller then refreshes group lists on the frontend.
/// The threshold is persisted on the source so the background sync keeps
/// re-clustering at the same value (never silently reverting to the default).
#[tauri::command]
pub fn recluster_source(
    source_id: i64,
    threshold: f64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    crate::clustering::recluster_l2(&state.db, source_id, threshold)
        .map_err(|e| e.to_string())?;
    state
        .db
        .set_source_threshold(source_id, threshold)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct ManualGroupResult {
    pub group_key: String,
    pub moved: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MergeGroupsArgs {
    pub level: u8,
    pub from_keys: Vec<String>,
    /// Required: group keys are shared across sources (workflow / model
    /// chain identities are not source-namespaced), so an unscoped merge
    /// would silently sweep in images from other sources holding the same
    /// key. The UI disables merging in the "所有源" view.
    pub source_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SplitImagesArgs {
    pub level: u8,
    pub image_ids: Vec<i64>,
}

#[tauri::command]
pub fn list_move_targets(
    source_id: i64,
    current_l2: String,
    state: State<'_, AppState>,
) -> Result<Vec<crate::db::GroupInfo>, String> {
    state.db.list_l2_targets(source_id, &current_l2).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoveImagesArgs { pub image_ids: Vec<i64>, pub target_group_key: String }

#[tauri::command]
pub fn move_images_to_group(args: MoveImagesArgs, state: State<'_, AppState>) -> Result<ManualGroupResult, String> {
    if args.image_ids.is_empty() { return Ok(ManualGroupResult { group_key: String::new(), moved: 0 }); }
    let info = state.db.image_move_context(&args.image_ids).map_err(|e| e.to_string())?;
    if info.iter().any(|(source, _, l2)| *source != info[0].0 || l2 == &args.target_group_key) {
        return Err("只能在同一来源目录下移动到其他 Prompt偏差组".into());
    }
    if !state.db.l2_target_exists(info[0].0, &args.target_group_key).map_err(|e| e.to_string())? {
        return Err("目标 Prompt偏差组不存在".into());
    }
    let moved = state.db.pin_images(2, &args.image_ids, &args.target_group_key, "move").map_err(|e| e.to_string())?;
    Ok(ManualGroupResult { group_key: args.target_group_key, moved })
}

#[tauri::command]
pub fn undo_split(group_key: String, source_id: i64, state: State<'_, AppState>) -> Result<usize, String> {
    state.db.undo_manual_split(&group_key, source_id).map_err(|e| e.to_string())
}

/// Manually merge two+ L2 (Prompt偏差) groups into one canonical group.
/// Every member image is pinned via `manual_group_bindings` and its
/// `group_key_l2` rewritten, so future re-clustering keeps them together
/// even if the Jaccard threshold would split them again.
#[tauri::command]
pub fn merge_groups(
    args: MergeGroupsArgs,
    state: State<'_, AppState>,
) -> Result<ManualGroupResult, String> {
    if args.level != 2 {
        return Err("暂仅支持 L2（Prompt偏差）级别的手动合并".to_string());
    }
    if args.source_id.is_none() {
        return Err("请先选择具体来源目录，再执行合并（避免误并其他来源的图片）".to_string());
    }
    let ids = state
        .db
        .image_ids_for_keys(args.level, &args.from_keys, args.source_id)
        .map_err(|e| e.to_string())?;
    if ids.is_empty() {
        return Ok(ManualGroupResult {
            group_key: String::new(),
            moved: 0,
        });
    }
    let prompts = state.db.prompts_for_ids(&ids).map_err(|e| e.to_string())?;
    let to_key = crate::clustering::manual_key(&prompts);
    let moved = state
        .db
        .pin_images(args.level, &ids, &to_key, "merge")
        .map_err(|e| e.to_string())?;
    Ok(ManualGroupResult {
        group_key: to_key,
        moved,
    })
}

/// Manually split selected images out of the current L2 group into a new
/// group. The pulled-out images are pinned so re-clustering never re-absorbs
/// them into the merged group they came from.
#[tauri::command]
pub fn split_images(
    args: SplitImagesArgs,
    state: State<'_, AppState>,
) -> Result<ManualGroupResult, String> {
    if args.level != 2 {
        return Err("暂仅支持 L2（Prompt偏差）级别的手动拆组".to_string());
    }
    if args.image_ids.is_empty() {
        return Ok(ManualGroupResult {
            group_key: String::new(),
            moved: 0,
        });
    }
    let prompts = state
        .db
        .prompts_for_ids(&args.image_ids)
        .map_err(|e| e.to_string())?;
    let new_key = crate::clustering::manual_key(&prompts);
    let moved = state
        .db
        .pin_images(args.level, &args.image_ids, &new_key, "split")
        .map_err(|e| e.to_string())?;
    Ok(ManualGroupResult {
        group_key: new_key,
        moved,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnmergeGroupArgs {
    pub group_key: String,
    /// Optional: restrict the undo to one source so a group whose key
    /// accidentally spans sources is only rolled back where asked.
    pub source_id: Option<i64>,
}

/// Undo a manual L2 merge from inside the merged group's folder view: every
/// member is restored to the L2 key it held before the merge and its binding
/// is removed, so auto re-clustering may place it again. The merged group
/// itself disappears (its members move back to their original groups).
#[tauri::command]
pub fn unmerge_group(
    args: UnmergeGroupArgs,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    state
        .db
        .unmerge_group(&args.group_key, args.source_id)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArenaHideArgs {
    pub group_key: String,
    pub survivor_id: i64,
    pub victim_id: i64,
    pub session_id: Option<String>,
    pub started_at: Option<String>,
    pub context_signature: Option<String>,
}

/// Arena hide: hide `victim` (score 0, excluded from scoring) and credit
/// `survivor` exactly like an arena winner. One review action snapshots both
/// images so a single undo restores the whole pair.
#[tauri::command]
pub fn arena_hide(
    args: ArenaHideArgs,
    state: State<'_, AppState>,
) -> Result<crate::db::ArenaHideResult, String> {
    state
        .db
        .arena_hide_atomic(
            &args.group_key,
            args.survivor_id,
            args.victim_id,
            args.session_id.as_deref(),
            args.started_at.as_deref(),
            args.context_signature.as_deref(),
        )
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportArgs {
    pub source_id: Option<i64>,
    pub format: String,
    pub dest: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportedImageRow {
    pub filename: String,
    pub abs_path: String,
    pub source: String,
    pub prompt_positive: String,
    pub prompt_negative: String,
    pub model: String,
    pub loras: String,
    pub vae: String,
    pub sampler: String,
    pub seed: i64,
    pub steps: i64,
    pub cfg: f64,
    pub group_key: String,
    pub labels: String,
    pub score: Option<f64>,
    pub notes: String,
    pub scanned_at: String,
}

#[tauri::command]
pub fn export_data(args: ExportArgs, state: State<'_, AppState>) -> Result<usize, String> {
    let conn = state.db.0.lock().unwrap();
    let sql = match args.source_id {
        Some(sid) => format!(
            "SELECT i.id, i.filename, i.abs_path, s.path, i.prompt_pos, i.prompt_neg,
                    i.checkpoint, i.loras, i.vae, i.samplers, i.seed, i.steps, i.cfg,
                    i.group_key_l3, i.source_kind, i.scanned_at
             FROM images i JOIN sources s ON s.id=i.source_id WHERE i.source_id={} ORDER BY i.id",
            sid
        ),
        None => String::from(
            "SELECT i.id, i.filename, i.abs_path, s.path, i.prompt_pos, i.prompt_neg,
                    i.checkpoint, i.loras, i.vae, i.samplers, i.seed, i.steps, i.cfg,
                    i.group_key_l3, i.source_kind, i.scanned_at
             FROM images i JOIN sources s ON s.id=i.source_id ORDER BY i.id",
        ),
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, i64>(11)?,
                r.get::<_, f64>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, String>(14)?,
                r.get::<_, String>(15)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut export_rows: Vec<ExportedImageRow> = Vec::new();
    for r in rows {
        let r = r.map_err(|e| e.to_string())?;
        let score: Option<f64> = conn
            .query_row(
                "SELECT internal_score FROM scores WHERE image_id=?1",
                rusqlite::params![r.0],
                |row| row.get(0),
            )
            .ok();
        let mut lstmt = conn
            .prepare("SELECT l.name FROM labels l JOIN image_labels il ON il.label_id=l.id WHERE il.image_id=?1")
            .map_err(|e| e.to_string())?;
        let lrows = lstmt
            .query_map(rusqlite::params![r.0], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut labels = Vec::new();
        for lr in lrows {
            labels.push(lr.map_err(|e| e.to_string())?);
        }
        let sampler_name = serde_json::from_str::<serde_json::Value>(&r.9)
            .ok()
            .and_then(|v| v.get(0).and_then(|x| x.get("sampler")).and_then(|x| x.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        export_rows.push(ExportedImageRow {
            filename: r.1,
            abs_path: r.2,
            source: r.3,
            prompt_positive: r.4,
            prompt_negative: r.5,
            model: r.6,
            loras: r.7,
            vae: r.8,
            sampler: sampler_name,
            seed: r.10,
            steps: r.11,
            cfg: r.12,
            group_key: r.13,
            labels: labels.join(";"),
            score,
            notes: String::new(),
            scanned_at: r.15,
        });
    }
    drop(stmt);
    drop(conn);

    let dest = PathBuf::from(&args.dest);
    let count = export_rows.len();
    if args.format.eq_ignore_ascii_case("json") {
        let json = serde_json::to_string_pretty(&export_rows).map_err(|e| e.to_string())?;
        std::fs::write(&dest, json).map_err(|e| e.to_string())?;
    } else {
        let mut csv = String::from(
            "filename,abs_path,source,prompt_positive,prompt_negative,model,loras,vae,sampler,seed,steps,cfg,group_key,labels,score,notes,scanned_at\n",
        );
        for r in &export_rows {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&r.filename),
                csv_escape(&r.abs_path),
                csv_escape(&r.source),
                csv_escape(&r.prompt_positive),
                csv_escape(&r.prompt_negative),
                csv_escape(&r.model),
                csv_escape(&r.loras),
                csv_escape(&r.vae),
                csv_escape(&r.sampler),
                r.seed,
                r.steps,
                r.cfg,
                csv_escape(&r.group_key),
                csv_escape(&r.labels),
                r.score.map(|s| format!("{:.2}", s)).unwrap_or_default(),
                csv_escape(&r.notes),
                csv_escape(&r.scanned_at),
            ));
        }
        std::fs::write(&dest, csv).map_err(|e| e.to_string())?;
    }
    Ok(count)
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveArgs {
    pub image_ids: Vec<i64>,
    pub dest_dir: String,
    pub organize: Option<String>,
}

#[tauri::command]
pub fn archive_copy(args: ArchiveArgs, state: State<'_, AppState>) -> Result<usize, String> {
    let mut ok = 0usize;
    for id in &args.image_ids {
        let (abs, organize_dir) = {
            let conn = state.db.0.lock().unwrap();
            let abs: String = conn
                .query_row(
                    "SELECT abs_path FROM images WHERE id=?1",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let mut dest_dir = PathBuf::from(&args.dest_dir);
            match args.organize.as_deref().unwrap_or("flat") {
                "checkpoint" => {
                    let ck: String = conn
                        .query_row(
                            "SELECT checkpoint FROM images WHERE id=?1",
                            rusqlite::params![id],
                            |r| r.get(0),
                        )
                        .unwrap_or_default();
                    let ck_safe = sanitize(&ck);
                    if !ck_safe.is_empty() {
                        dest_dir = dest_dir.join(ck_safe);
                    }
                }
                "label" => {
                    let mut lstmt = conn
                        .prepare("SELECT l.name FROM labels l JOIN image_labels il ON il.label_id=l.id WHERE il.image_id=?1")
                        .map_err(|e| e.to_string())?;
                    let lrows = lstmt
                        .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                        .map_err(|e| e.to_string())?;
                    let mut labels = Vec::new();
                    for lr in lrows {
                        labels.push(lr.map_err(|e| e.to_string())?);
                    }
                    drop(lstmt);
                    let label_dir = if labels.is_empty() {
                        "unlabeled".to_string()
                    } else {
                        sanitize(&labels.join("_"))
                    };
                    dest_dir = dest_dir.join(label_dir);
                }
                _ => {}
            }
            (abs, dest_dir)
        };
        let src = PathBuf::from(&abs);
        if !src.exists() {
            continue;
        }
        let target = archive::copy_to(&src, &organize_dir).map_err(|e| e.to_string())?;
        state
            .db
            .record_archive(*id, &target.to_string_lossy())
            .map_err(|e| e.to_string())?;
        ok += 1;
    }
    Ok(ok)
}

fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == ' ')
        .collect::<String>()
        .trim()
        .replace(' ', "_")
}

#[tauri::command]
pub fn recommend_prompts(
    group_key: String,
    granularity: u8,
    offset: i64,
    limit: i64,
    state: State<'_, AppState>,
) -> Result<Vec<crate::db::PromptRecommendation>, String> {
    state
        .db
        .recommend_prompts(&group_key, granularity, offset, limit)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetryEventArgs {
    #[serde(default)]
    pub session_id: Option<String>,
    pub event_name: String,
    pub schema_version: String,
    #[serde(default)]
    pub occurred_at: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    pub payload_json: String,
    #[serde(default)]
    pub severity: Option<String>,
}

#[tauri::command]
pub fn record_telemetry_event(
    args: TelemetryEventArgs,
    state: State<'_, AppState>,
) -> Result<crate::db::TelemetryEventResult, String> {
    state
        .db
        .record_telemetry_event(
            args.session_id.as_deref(),
            &args.event_name,
            &args.schema_version,
            args.occurred_at.as_deref(),
            args.mode.as_deref(),
            &args.payload_json,
            args.severity.as_deref(),
        )
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn export_diagnostics(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    state.db.export_diagnostics(1000).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn db_path(app: tauri::AppHandle) -> String {
    let dir = app
        .path()
        .app_data_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "app_data".to_string());
    let _ = std::fs::create_dir_all(&dir);
    PathBuf::from(&dir)
        .join("ai-image-sorter.db")
        .to_string_lossy()
        .to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupThumbDto {
    pub group_key: String,
    pub thumb_paths: Vec<String>,
}

#[tauri::command]
pub fn get_group_thumbnails(
    group_keys: Vec<String>,
    level: u8,
    state: State<'_, AppState>,
) -> Result<Vec<GroupThumbDto>, String> {
    let mut out = Vec::new();
    for gk in group_keys {
        let paths = state
            .db
            .list_group_thumbnail_paths(&gk, level)
            .map_err(|e| e.to_string())?;
        if !paths.is_empty() {
            out.push(GroupThumbDto {
                group_key: gk,
                thumb_paths: paths,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod e2e_tests {
    use super::*;

    /// End-to-end validation against a COPY of the real local database
    /// (skips gracefully when absent). Verifies the v9 migration, reparse
    /// backfill of broken metadata, workflow re-keying and template
    /// matching against the actual ComfyUI workflow files.
    #[test]
    fn real_db_backfill_and_workflow_matching() {
        let Some(appdata) = std::env::var_os("APPDATA") else { return };
        let real = PathBuf::from(&appdata)
            .join("com.aiimagesorter.app")
            .join("ai-image-sorter.db");
        if !real.exists() {
            eprintln!("no real DB at {real:?}, skipping e2e test");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ais-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("ai-image-sorter.db");
        std::fs::copy(&real, &copy).unwrap();
        let db = crate::db::open(&copy).unwrap();

        let source_total = db.list_sources().unwrap().len();
        assert!(source_total > 0, "real DB must have sources");
        let sources = db.list_sources().unwrap();
        let _ = refresh_templates(&db).expect("template refresh");

        let mut matched_any = false;
        for source in &sources {
            let norm = normalize_path(&PathBuf::from(&source.path));
            let (files, _) = comfy_finder::list_images_with_newest(&PathBuf::from(&norm));
            let stats = scan_source(&db, source.id, &norm, &files).unwrap();
            let (backfilled, _) = backfill_source(&db, source.id, &norm).unwrap();
            let mut affected = stats.affected.clone();
            affected.extend(backfilled);
            if !affected.is_empty() {
                crate::clustering::recluster_l2_keys(
                    &db,
                    source.id,
                    &affected,
                    crate::clustering::DEFAULT_L2_THRESHOLD,
                )
                .unwrap();
            }
        }
        let assigned = match_templates(&db).unwrap();
        // Steady-state DBs already have every workflow matched (from the
        // desktop app), so `assigned` can be 0 while matches still exist.
        // The `matched` count is computed below from the DB itself.

        // 1. Broken rows repaired: every Comfy image has a checkpoint or a
        //    workflow key after backfill.
        let broken = db
            .list_images_needing_reparse(source_total as i64 + 1)
            .unwrap()
            .len();
        let conn = db.0.lock().unwrap();
        let empty_ck: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM images WHERE (checkpoint IS NULL OR checkpoint='') AND source_kind='comfy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let no_key: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM images WHERE workflow_key IS NULL OR workflow_key=''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let with_model: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM images WHERE diffusion_model IS NOT NULL AND diffusion_model!=''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let matched: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM images WHERE workflow_template_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(conn);

        // Steady-state DBs already have every workflow matched (from the
        // desktop app), so `assigned` can be 0 while matches still exist.
        matched_any = assigned > 0 || matched > 0;

        eprintln!(
            "e2e: total={} empty_ck={} no_key={} with_model={} template_matched={} assigned_keys={} broken_left={}",
            db.list_source_image_paths(1).map(|v| v.len()).unwrap_or(0) + broken,
            empty_ck,
            no_key,
            with_model,
            matched,
            assigned,
            broken,
        );

        let conn = db.0.lock().unwrap();
        let groups = {            let mut stmt = conn
                .prepare(
                    "SELECT i.group_key_l1, COUNT(*) c, MIN(wt.name) wname,
                            GROUP_CONCAT(DISTINCT COALESCE(NULLIF(i.diffusion_model,''),'(未知)'))
                     FROM images i LEFT JOIN workflow_templates wt ON wt.id=i.workflow_template_id
                     GROUP BY i.group_key_l1 ORDER BY c DESC",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })
                .unwrap();
            let mut out = Vec::new();
            for r in rows {
                out.push(r.unwrap());
            }
            out
        };
        drop(conn);
        for (key, count, name, models) in &groups {
            eprintln!("e2e group: {}x key={} name={:?} models={:?}", count, &key[..8.min(key.len())], name, models);
        }

        assert!(empty_ck == 0, "comfy images must not be model-less after backfill");
        assert!(no_key == 0, "no image may lack a workflow key after backfill");
        assert!(with_model > 0, "diffusion_model must be populated");
        assert!(matched_any, "at least one workflow key must match a template");

        // Prompt-parsing coverage on real data (diagnostic; no assert because
        // some pipelines legitimately encode no text at all).
        let conn = db.0.lock().unwrap();
        let prompt_stats: (i64, i64) = conn
            .query_row(
                "SELECT
                   SUM(CASE WHEN (prompt_pos IS NOT NULL AND prompt_pos!='') OR (prompt_neg IS NOT NULL AND prompt_neg!='') THEN 1 ELSE 0 END),
                   SUM(CASE WHEN meta_ok=1 AND source_kind='comfy'
                             AND (prompt_pos IS NULL OR prompt_pos='')
                             AND (prompt_neg IS NULL OR prompt_neg='') THEN 1 ELSE 0 END)
                 FROM images",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT filename, checkpoint, substr(prompt_pos,1,60) FROM images
                 WHERE meta_ok=1 AND source_kind='comfy'
                   AND (prompt_pos IS NULL OR prompt_pos='')
                   AND (prompt_neg IS NULL OR prompt_neg='')
                 ORDER BY filename LIMIT 12",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .collect::<Vec<_>>();
        let mut promptless = Vec::new();
        for (f, ck, pp) in rows.into_iter().flatten() {
            promptless.push(format!("{f} (ckpt={ck:?}, pos={pp:?})"));
        }
        drop(stmt);
        let civitai: Vec<(String, String, String, i64)> = {
            let mut s = conn
                .prepare(
                    "SELECT filename,
                            COALESCE(substr(prompt_pos,1,50),''),
                            COALESCE(samplers,''),
                            (samplers IS NOT NULL AND samplers!='' AND samplers!='[]') AS has_samplers
                     FROM images WHERE filename LIKE 'Krea2_2026%' ORDER BY filename",
                )
                .unwrap();
            s.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };
        drop(conn);
        eprintln!(
            "e2e prompt: with_prompt={} comfy_without_prompt={}",
            prompt_stats.0, prompt_stats.1
        );
        for p in promptless {
            eprintln!("e2e promptless: {p}");
        }
        for (f, pos, samplers, has) in &civitai {
            eprintln!("e2e civitai: {f} pos={pos:?} samplers={} has_samplers={has}", samplers.len());
        }
        for (_, pos, _, has) in &civitai {
            assert!(!pos.is_empty(), "CivitAI image must have a prompt after reparse");
            assert!(*has == 1, "CivitAI image must have samplers after reparse");        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Parse the real CivitAI folder images directly with `parse_file` and
    /// assert prompt + sampler extraction (skips gracefully when absent).
    #[test]
    fn real_civitai_images_parse_prompts() {
        let dir = r"C:\cc\ComfyUI_windows_portable\ComfyUI\output\CivitAI";
        if !std::path::Path::new(dir).is_dir() {
            eprintln!("no CivitAI folder, skipping");
            return;
        }
        let mut checked = 0;
        for entry in walkdir::WalkDir::new(dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if !p.is_file()
                || p.extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_lowercase())
                    .as_deref()
                    != Some("png")
            {
                continue;
            }
            let meta = crate::metadata::parse_file(p).unwrap();
            checked += 1;
            eprintln!(
                "civitai real parse: {} prompt_len={} samplers={} ckpt={}",
                p.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                meta.prompt_pos.len(),
                meta.samplers.len(),
                meta.checkpoint,
            );
            assert!(!meta.prompt_pos.is_empty(), "{}: prompt must parse", p.display());
            assert!(!meta.samplers.is_empty(), "{}: samplers must parse", p.display());
            assert_eq!(meta.source_kind, crate::metadata::SourceKind::Comfy);
        }
        assert!(checked >= 1, "expected at least one CivitAI PNG");
    }
}
