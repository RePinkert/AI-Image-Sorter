use crate::db::{ImageInsert, ImageRow, LabelRow, SourceRow};
use crate::grouper::compute_group_keys;
use crate::metadata::{parse_file, ImageMeta};
use crate::{archive, comfy_finder, db::GroupInfo, scoring};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

pub struct AppState {
    pub db: Arc<crate::db::Db>,
    /// 视觉签名引擎（阶段六）。模型文件缺失时 model_path 指向一个不存在的
    /// 路径 —— 功能保持「不可用」而不是让应用起不来。
    pub vision: Arc<crate::vision::VisionEngine>,
    /// 文本塔引擎槽（阶段七轮 1 接线）。权重与分词器**缺任一份**就整体
    /// 不可用 —— 同样保持「不可用」而不是让应用起不来。
    pub text: Arc<crate::text::TextEngineSlot>,
    /// 当前批处理的取消标志（None = 没有在跑）。
    pub signature_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// 文本塔批处理的取消标志 —— 与视觉塔**分开**，两边的任务互不打断。
    pub text_signature_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateImagePromptsArgs {
    pub image_id: i64,
    pub prompt_pos: String,
    pub prompt_neg: String,
    pub expected_size: i64,
    pub expected_modified_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateImagePromptsResult {
    pub prompt_pos: String,
    pub prompt_neg: String,
    pub group_key_l2: String,
    pub group_key_l3: String,
    pub modified_at: i64,
    pub size: i64,
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
    /// v19：这一轮识别出的改名数量。
    pub renamed: usize,
    /// v19：身份无法确认、被保留待确认的旧记录数量。
    pub unconfirmed: usize,
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
    /// v19：识别出的改名数量（路径变了，行没换 → 评分/标签/分组全保住）。
    pub renamed: usize,
    /// v19：身份无法确认、被保留待确认的旧记录数量。
    pub unconfirmed: usize,
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
            renamed: stats.renamed.len(),
            unconfirmed: stats.unconfirmed.len(),
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
    /// v19：识别出的改名（路径变了，行没换）。
    renamed: Vec<RenameEvent>,
    /// v19：身份无法确认、但**带用户状态**因而被保留的旧记录。
    unconfirmed: Vec<UnconfirmedImage>,
}

/// 一次改名命中 —— 要把「凭什么认为是同一张图」如实告诉用户。
#[derive(Debug, Clone, Serialize)]
pub struct RenameEvent {
    pub old_path: String,
    pub new_path: String,
    /// 判定依据（内容键 / 元数据 / 内容哈希）。
    pub basis: String,
}

/// 身份无法确认的旧记录（文件不在原路径，且认不出来）。
#[derive(Debug, Clone, Serialize)]
pub struct UnconfirmedImage {
    pub abs_path: String,
    pub filename: String,
    pub reason: String,
}

/// Idempotent scan of one source folder: keep rows for files still on disk
/// (their id and thus their scores/labels survive), insert brand-new files
/// only, and delete rows whose abs_path no longer exists (cascades to
/// score/label). Files modified in the last `STABILITY_WINDOW_SECS` seconds
/// are deferred so partially-written files never become permanently broken
/// rows. Match on NORMALIZED paths so trailing slashes / case / separator
/// differences can never cause a file to be mis-detected as "new" — which
/// would re-allocate image_id and CASCADE-delete its scores.
///
/// **v19（第 29 轮）· 改名不丢分拣**：盘上路径对不上的旧行**先不删** ——
/// 先按**内容身份**尝试认领（尺寸+种子+prompt → 元数据 → 内容哈希），
/// 认领成功就**就地改路径**（image_id 不变 → 评分 / 标签 / 手动分组全部
/// 跟着走）；认领不出来的、带用户状态的行**保留**并上报，绝不静默删除。
///
/// `files` is the caller's one-pass walk result (paths already sorted), so
/// the changed-source sync path doesn't walk the tree a second time.
fn scan_source(
    db: &crate::db::Db,
    source_id: i64,
    norm_source: &str,
    files: &[PathBuf],
) -> Result<ScanStats, String> {
    let identity = db
        .list_source_identity_rows(source_id)
        .map_err(|e| e.to_string())?;
    let mut existing_map: HashMap<String, crate::db::IdentityRow> = HashMap::new();
    for row in &identity {
        existing_map.insert(normalize_path(&PathBuf::from(&row.abs_path)), row.clone());
    }
    // 带用户状态（评分 / 标签 / 手动分组）的行：改名认领失败时**保留**，
    // 交给用户确认 —— 宁可多留一行，也不毁掉手工分拣的结果。
    let user_state = db
        .images_with_user_state(source_id)
        .map_err(|e| e.to_string())?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut stats = ScanStats {
        found: files.len(),
        ..Default::default()
    };

    let mut on_disk: HashSet<String> = HashSet::new();
    // 待解析的新文件（路径对不上已有行，且不在稳定性窗口内）。
    let mut fresh: Vec<PathBuf> = Vec::new();
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
        if let Some(row) = existing_map.get(&norm) {
            // Keep the stable image id while refreshing file-level fields that
            // can change outside the app.
            let size = std::fs::metadata(f).map(|m| m.len() as i64).unwrap_or(0);
            let modified_at = file_mtime.unwrap_or(0);
            db.update_file_stats(row.id, size, modified_at)
                .map_err(|e| e.to_string())?;
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
        fresh.push(f.clone());
    }

    // 解析新文件（顺带算内容指纹 —— 改名判定与将来的内容级去重都要它）。
    struct Parsed {
        norm: String,
        filename: String,
        meta: ImageMeta,
        size: i64,
        mtime: i64,
        fp: String,
    }
    let mut parsed: Vec<Parsed> = Vec::new();
    for f in &fresh {
        let meta = match parse_file(f) {
            Ok(meta) => meta,
            Err(error) => {
                stats.parse_errors += 1;
                log::warn!("image metadata parse failed for source {source_id}: {error}");
                ImageMeta::default()
            }
        };
        let size = std::fs::metadata(f).map(|m| m.len() as i64).unwrap_or(0);
        let mtime = std::fs::metadata(f)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let fp = crate::fingerprint::file_fingerprint(f).unwrap_or_default();
        parsed.push(Parsed {
            norm: normalize_path(f),
            filename: f
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            meta,
            size,
            mtime,
            fp,
        });
    }

    // ── 改名识别 ────────────────────────────────────────────────
    // 旧行 = 库里还有、但路径不在盘上的行。
    let missing: Vec<&crate::db::IdentityRow> = identity
        .iter()
        .filter(|r| !on_disk.contains(&normalize_path(&PathBuf::from(&r.abs_path))))
        .collect();
    let olds: Vec<crate::fingerprint::RenameOld> = missing
        .iter()
        .map(|r| crate::fingerprint::RenameOld {
            id: r.id,
            abs_path: r.abs_path.clone(),
            size: r.size,
            seed: r.seed,
            prompt_pos: r.prompt_pos.clone(),
            prompt_neg: r.prompt_neg.clone(),
            meta_sig: r.meta_sig(),
            content_fp: r.content_fp.clone(),
        })
        .collect();
    let news: Vec<crate::fingerprint::RenameCandidate> = parsed
        .iter()
        .map(|p| {
            let sampler = p.meta.samplers.first();
            crate::fingerprint::RenameCandidate {
                size: p.size,
                seed: sampler.map(|s| s.seed).unwrap_or(0),
                prompt_pos: p.meta.prompt_pos.clone(),
                prompt_neg: p.meta.prompt_neg.clone(),
                meta_sig: crate::fingerprint::meta_signature(
                    &p.meta.workflow_key,
                    p.meta.width as i64,
                    p.meta.height as i64,
                    sampler.map(|s| s.steps).unwrap_or(0),
                    sampler.map(|s| s.cfg).unwrap_or(0.0),
                    &p.meta.recipe_signature(),
                ),
                content_fp: p.fp.clone(),
            }
        })
        .collect();
    let matches = crate::fingerprint::match_renames(&olds, &news);

    let mut matched_new: HashSet<usize> = HashSet::new();
    let mut preserved: HashSet<i64> = HashSet::new();
    for m in &matches {
        let old = &olds[m.old_index];
        let new = &parsed[m.new_index];
        // 就地改路径 —— image_id 不变，评分 / 标签 / 手动分组 / 向量全保住。
        db.rename_image_path(old.id, &new.norm, &new.filename, &new.fp)
            .map_err(|e| e.to_string())?;
        matched_new.insert(m.new_index);
        stats.kept += 1;
        if new.mtime > stats.newest_mtime {
            stats.newest_mtime = new.mtime;
        }
        stats.renamed.push(RenameEvent {
            old_path: old.abs_path.clone(),
            new_path: new.norm.clone(),
            basis: m.basis.as_str().to_string(),
        });
    }
    // 认领不出来的旧行：带用户状态的**保留**并上报，等用户确认。
    for (i, o) in olds.iter().enumerate() {
        if matches.iter().any(|m| m.old_index == i) {
            continue;
        }
        if user_state.contains(&o.id) {
            preserved.insert(o.id);
            stats.unconfirmed.push(UnconfirmedImage {
                abs_path: o.abs_path.clone(),
                filename: o.abs_path.rsplit('/').next().unwrap_or("").to_string(),
                reason: "文件已不在原路径，且无法按内容确认身份 —— 记录已保留".to_string(),
            });
        }
    }

    // ── 认领不出来的旧文件 = 真·新图 ──────────────────────────
    for (i, p) in parsed.iter().enumerate() {
        if matched_new.contains(&i) {
            continue;
        }
        let dimension = db.get_source_dimension(source_id).unwrap_or_else(|_| crate::grouper::DEFAULT_DIMENSION.into());
        let keys = compute_group_keys(&p.meta, norm_source, &dimension);
        stats.affected.insert(keys.l1.clone());
        if p.mtime > stats.newest_mtime {
            stats.newest_mtime = p.mtime;
        }
        let seed = p.meta.samplers.first().map(|s| s.seed).unwrap_or(0);
        let steps = p.meta.samplers.first().map(|s| s.steps).unwrap_or(0);
        let cfg = p.meta.samplers.first().map(|s| s.cfg).unwrap_or(0.0);
        // Store the NORMALIZED abs_path so future rescans match this row by
        // the same normalization function, regardless of how the user
        // re-picked the source folder.
        let abs = p.norm.clone();

        let row = ImageInsert {
            source_id,
            rel_path: abs.clone(),
            abs_path: abs,
            filename: p.filename.clone(),
            size: p.size,
            modified_at: p.mtime,
            width: p.meta.width as i64,
            height: p.meta.height as i64,
            prompt_pos: p.meta.prompt_pos.clone(),
            prompt_neg: p.meta.prompt_neg.clone(),
            checkpoint: p.meta.checkpoint.clone(),
            loras: serde_json::to_string(&p.meta.loras).unwrap_or_default(),
            vae: p.meta.vae.clone(),
            text_encoder: p.meta.text_encoder.clone(),
            controlnet: p.meta.controlnet.clone(),
            samplers: serde_json::to_string(&p.meta.samplers).unwrap_or_default(),
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
            // v19: 内容指纹（改名认领与将来的内容级去重用它）。
            content_fp: p.fp.clone(),
            meta_ok: p.meta.raw_ok,
            source_kind: format!("{:?}", p.meta.source_kind).to_lowercase(),
            diffusion_model: p.meta.diffusion_model.clone(),
            model_chain_json: serde_json::to_string(&p.meta.model_chain)
                .unwrap_or_else(|_| "[]".to_string()),
            workflow_key: p.meta.workflow_key.clone(),
            workflow_graph_json: p.meta.workflow_graph_json.clone(),
            workflow_template_id: None,
            workflow_match_confidence: None,
            generation_recipe_json: p.meta.generation_recipe_json(),
            recipe_signature: p.meta.recipe_signature(),
            parser_version: crate::metadata::PARSER_VERSION.to_string(),
        };
        db.insert_image(&row).map_err(|e| e.to_string())?;
        stats.added += 1;
    }

    // Remove rows for files that vanished from disk — except the ones the
    // user still has state on (they are preserved for confirmation).
    let (removed, deleted_l1) = db
        .delete_missing_source_images(source_id, &on_disk, &preserved)
        .map_err(|e| e.to_string())?;
    stats.affected.extend(deleted_l1);
    let _ = removed;
    Ok(stats)
}

/// 补算**内容指纹**（第 29 轮）。
///
/// 指纹是「改名认领」的第三级判定依据。它必须在**改名之前**就存在 —— 改名
/// 之后旧文件已经不在盘上，没法再算它的指纹了。
///
/// 代价：把该源所有还没指纹的文件读一遍（实测 957 张 / 1.71 GB ≈ 3.0 s），
/// **一次性** —— 补齐之后每次扫描都是 0 成本。
fn backfill_content_fingerprints(
    db: &crate::db::Db,
    source_id: i64,
) -> Result<usize, String> {
    let rows = db
        .list_source_identity_rows(source_id)
        .map_err(|e| e.to_string())?;
    let mut done = 0usize;
    for row in rows {
        if !row.content_fp.is_empty() {
            continue;
        }
        let path = PathBuf::from(&row.abs_path);
        if !path.is_file() {
            continue;
        }
        if let Some(fp) = crate::fingerprint::file_fingerprint(&path) {
            db.set_content_fp(row.id, &fp).map_err(|e| e.to_string())?;
            done += 1;
        }
    }
    if done > 0 {
        log::info!("source {source_id}: 补算内容指纹 {done} 张");
    }
    Ok(done)
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
        let dimension = db.get_source_dimension(source_id).unwrap_or_else(|_| crate::grouper::DEFAULT_DIMENSION.into());
        let keys = compute_group_keys(&meta, norm_source, &dimension);
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
            text_encoder: meta.text_encoder.clone(),
            controlnet: meta.controlnet.clone(),
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
            content_fp: String::new(),
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
    let mut total_renamed = 0usize;
    let mut total_unconfirmed = 0usize;
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
        // 阶段六：聚类方式由用户决定（词匹配 / 视觉 / 语义）。
        // 语义模式的阈值是**余弦**，量纲与 Jaccard 完全不同 → 单独存。
        let l2_mode = crate::clustering::L2Mode::parse(
            &db.get_source_l2_mode(src.id).unwrap_or_else(|_| "text".into()),
        );
        let l2_visual_threshold = db
            .get_source_visual_threshold(src.id)
            .unwrap_or(crate::clustering::DEFAULT_VISUAL_THRESHOLD);
        // 语义档（BGE）的阈值是**另一个量纲**，单独存（v18 迁移）。
        // 0.75 是 CLIP 的值，绝不可沿用到 BGE。
        let l2_semantic_threshold = db
            .get_source_semantic_threshold(src.id)
            .unwrap_or(crate::clustering::DEFAULT_SEMANTIC_THRESHOLD);
        let mode_threshold = match l2_mode {
            crate::clustering::L2Mode::Semantic => l2_semantic_threshold,
            _ => l2_visual_threshold,
        };

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
            total_renamed += stats.renamed.len();
            total_unconfirmed += stats.unconfirmed.len();
            emit("scan", found, processed, added, pending, stats.parse_errors);
            // 指纹一次性补齐 —— 必须在**下一次改名之前**算好。
            let _ = backfill_content_fingerprints(db, src.id);
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
            match l2_mode {
                crate::clustering::L2Mode::Text => {
                    crate::clustering::recluster_l2_keys(
                        db,
                        src.id,
                        &affected,
                        l2_threshold,
                    )
                    .map_err(|e| e.to_string())?;
                }
                mode => {
                    // 语义/视觉模式：只重算受影响的 L1 桶。
                    // 槽位与模型一律走 `signature_spec`（口径收口，防错空间）。
                    let Some((slot, model_id)) = mode.signature_spec() else {
                        return Err("未知的 L2 聚类档位".into());
                    };
                    crate::clustering::recluster_l2_vectors(
                        db,
                        src.id,
                        slot,
                        model_id,
                        mode_threshold,
                        Some(&affected),
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            any_recluster = true;
        }
        // ── v19 / 第 30 轮：这里**不再**偷偷补算签名 ──────────────
        //
        // 旧版（阶段六~七）在同步循环里顺手给缺向量的图补算签名，每批 64 张，
        // 算完立刻**全量重聚类**整个库（`recluster_l2_vectors(..., None)`），
        // 然后回报 `reclustered=true` → 前端把它渲染成「同步完成：已检查 1 个
        // 目录」，3 秒后消失 → **每 8 秒闪一次**（实测：2026-10-26 18:34 的
        // 五次 sync 事件与五个 64 张的签名批次一一对应）。
        //
        // 三条理由删掉它：
        // 1. **未经批准就烧 CPU** —— 违反项目自己的口径 #2「代价必须由用户
        //    批准」。视觉推理约 3 张/秒，957 张要 5 分钟。
        // 2. **每批都全量重聚类**（1.5~2.5 s）**并作废全站缩略图**。
        // 3. **它报的是假话** —— 「同步完成」跟「后台在补算向量」毫无关系。
        //
        // 现在的口径：缺向量的图**保持原组**（纪律 #6：不做无声的搬动），
        // 由界面**如实告诉用户**「有 N 张还没有向量，暂未参与聚类」，计算
        // 只走设置页那个显式按钮（带进度、可取消）。
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
        renamed: total_renamed,
        unconfirmed: total_unconfirmed,
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
    let allowed = crate::grouper::DIMENSIONS;
    if config.dimensions.iter().any(|d| !allowed.contains(&d.as_str())) {
        return Err("包含不支持的 Model偏差维度".into());
    }
    state.db.set_model_deviation_dimensions(config.source_id, &config.dimensions)
        .map_err(|e| e.to_string())
}

/// Wheel-driven alias: persist the SINGLE selected dimension, re-derive the
/// source's L1/L3 fingerprints from stored raw columns (exact-match, no
/// reparse), and re-cluster L2 on top of the new L1. Returns the resulting
/// group count at level 1.
#[tauri::command]
pub fn recluster_l1(
    source_id: i64,
    dimension: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    if !crate::grouper::DIMENSIONS.contains(&dimension.as_str()) {
        return Err(format!("未知的 Model偏差维度：{dimension}"));
    }
    if dimension == "lora" {
        // The lora dimension is a DERIVED per-single-LoRA membership view
        // (json_each over the `loras` JSON). It neither rewrites
        // group_key_l1 nor reclusters L2, so switching back to any other
        // dimension is possible without data loss.
        state.db
            .set_model_deviation_dimensions(source_id, std::slice::from_ref(&dimension))
            .map_err(|e| e.to_string())?;
        let groups = state.db.list_groups(Some(source_id), 1).map_err(|e| e.to_string())?;
        return Ok(groups.len());
    }
    state.db.rehash_l1_for_source(source_id, &dimension)
        .map_err(|e| e.to_string())?;
    state.db
        .set_model_deviation_dimensions(source_id, std::slice::from_ref(&dimension))
        .map_err(|e| e.to_string())?;
    let threshold = state.db.get_source_threshold(source_id).unwrap_or(crate::clustering::DEFAULT_L2_THRESHOLD);
    crate::clustering::recluster_l2(&state.db, source_id, threshold)
        .map_err(|e| e.to_string())?;
    let groups = state.db.list_groups(Some(source_id), 1).map_err(|e| e.to_string())?;
    Ok(groups.len())
}

#[tauri::command(async)]
pub fn list_groups(
    source_id: Option<i64>,
    level: u8,
    state: State<'_, AppState>,
) -> Result<Vec<GroupInfo>, String> {
    state.db.list_groups(source_id, level).map_err(|e| e.to_string())
}

#[tauri::command(async)]
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

/// Hide / un-hide every member image of an L2 (Prompt偏差) group in one
/// action. Used by the expandable list view's ⊗ button.
#[tauri::command(async)]
pub fn hide_group_images(
    source_id: i64,
    group_key: String,
    hidden: bool,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    state
        .db
        .set_group_hidden(2, &group_key, source_id, hidden)
        .map_err(|e| e.to_string())
}

/// Shift+Delete semantics for a whole L2 group: send every member image to
/// the OS recycle bin (recoverable from the desktop Recycle Bin) and remove
/// its DB row. Returns how many images were deleted.
#[tauri::command(async)]
pub fn trash_group_images(
    source_id: i64,
    group_key: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let ids = state
        .db
        .image_ids_for_keys(2, &[group_key.clone()], Some(source_id))
        .map_err(|e| e.to_string())?;
    let mut deleted = 0usize;
    for id in ids {
        // trash_image is wrapped in a Tauri command; replicate its body here
        // so one failure doesn't abort the remaining members.
        match trash_one_image(&state.db, id) {
            Ok(()) => deleted += 1,
            Err(_) => continue,
        }
    }
    Ok(deleted)
}

fn trash_one_image(db: &crate::db::Db, image_id: i64) -> Result<(), String> {
    let abs = db
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
    db.delete_image_by_id(image_id)
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

#[tauri::command(async)]
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

#[tauri::command]
pub fn update_image_prompts(
    args: UpdateImagePromptsArgs,
    state: State<'_, AppState>,
) -> Result<UpdateImagePromptsResult, String> {
    let (abs, source_id, source_path, old_l1, threshold) = state
        .db
        .image_edit_target(args.image_id)
        .map_err(|e| e.to_string())?;
    let path = PathBuf::from(&abs);
    if path.extension().and_then(|e| e.to_str()).map(|e| !e.eq_ignore_ascii_case("png")).unwrap_or(true) {
        return Err("暂不支持：首版仅支持 PNG 图片".into());
    }
    let stat = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    let current_size = stat.len() as i64;
    let current_mtime = stat.modified().ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64).unwrap_or(0);
    if current_size != args.expected_size || current_mtime != args.expected_modified_at {
        return Err("图片已被其他程序修改，请刷新后重试".into());
    }

    let temp = crate::metadata::png_edit::write_prompts(&path, &args.prompt_pos, &args.prompt_neg)
        .map_err(|e| e.to_string())?;
    let parsed = parse_file(&temp).map_err(|e| { let _ = std::fs::remove_file(&temp); e.to_string() })?;
    if parsed.prompt_pos != args.prompt_pos.trim() || parsed.prompt_neg != args.prompt_neg.trim() {
        let _ = std::fs::remove_file(&temp);
        return Err("写回验证失败：图片中的 Prompt 与输入不一致".into());
    }

    let backup = path.with_extension("png.ai-image-sorter-backup");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&path, &backup).map_err(|e| { let _ = std::fs::remove_file(&temp); e.to_string() })?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::rename(&backup, &path);
        let _ = std::fs::remove_file(&temp);
        return Err(error.to_string());
    }

    let update_result = (|| -> Result<(), String> {
        let meta = parse_file(&path).map_err(|e| e.to_string())?;
        if meta.prompt_pos != args.prompt_pos.trim() || meta.prompt_neg != args.prompt_neg.trim() {
            return Err("最终文件 Prompt 验证失败".into());
        }
        let norm_source = normalize_path(&PathBuf::from(&source_path));
        let dimension = state.db.get_source_dimension(source_id).unwrap_or_else(|_| crate::grouper::DEFAULT_DIMENSION.into());
        let keys = compute_group_keys(&meta, &norm_source, &dimension);
        let stat = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        let modified_at = stat.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64).unwrap_or(0);
        let sampler = meta.samplers.first().cloned().unwrap_or_default();
        let row = ImageInsert {
            source_id, rel_path: abs.clone(), abs_path: abs.clone(),
            filename: path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
            size: stat.len() as i64, modified_at, width: meta.width as i64, height: meta.height as i64,
            prompt_pos: meta.prompt_pos.clone(), prompt_neg: meta.prompt_neg.clone(), checkpoint: meta.checkpoint.clone(),
            loras: serde_json::to_string(&meta.loras).unwrap_or_default(), vae: meta.vae.clone(),
            text_encoder: meta.text_encoder.clone(), controlnet: meta.controlnet.clone(),
            samplers: serde_json::to_string(&meta.samplers).unwrap_or_default(), seed: sampler.seed, steps: sampler.steps, cfg: sampler.cfg,
            group_key_l0: keys.l0, group_key_l1: keys.l1.clone(), group_key_l2: keys.l2, group_key_l3: keys.l3,
            sha256: String::new(), meta_ok: meta.raw_ok, source_kind: format!("{:?}", meta.source_kind).to_lowercase(),
            diffusion_model: meta.diffusion_model.clone(), model_chain_json: serde_json::to_string(&meta.model_chain).unwrap_or_else(|_| "[]".into()),
            workflow_key: meta.workflow_key.clone(), workflow_graph_json: meta.workflow_graph_json.clone(),
            workflow_template_id: None, workflow_match_confidence: None,
            generation_recipe_json: meta.generation_recipe_json(), recipe_signature: meta.recipe_signature(),
            parser_version: crate::metadata::PARSER_VERSION.to_string(),
            content_fp: String::new(),
        };
        state.db.update_reparsed_image(args.image_id, &row, source_id).map_err(|e| e.to_string())?;
        // 文件内容被改写 → 旧的内容指纹失效了，重新算。
        if let Some(fp) = crate::fingerprint::file_fingerprint(&path) {
            let _ = state.db.set_content_fp(args.image_id, &fp);
        }
        let affected = HashSet::from([old_l1.clone(), keys.l1]);
        crate::clustering::recluster_l2_keys(&state.db, source_id, &affected, threshold).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = update_result {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::rename(&backup, &path);
        return Err(error);
    }
    let _ = std::fs::remove_file(&backup);
    let (prompt_pos, prompt_neg, group_key_l2, group_key_l3, modified_at, size) = state.db.image_prompt_groups(args.image_id).map_err(|e| e.to_string())?;
    Ok(UpdateImagePromptsResult { prompt_pos, prompt_neg, group_key_l2, group_key_l3, modified_at, size })
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

#[tauri::command(async)]
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

#[tauri::command(async)]
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
///
/// 阶段六：方式可切。`text` 是历史行为（词 Jaccard 单链）；`visual` / `semantic`
/// 用签名向量 + 平均连接。后两者的阈值是**余弦**，量纲与 Jaccard 完全不同，
/// 所以由调用方分别传入（`threshold` 是 Jaccard，`visual_threshold` 是余弦）。
#[tauri::command]
pub fn recluster_source(
    source_id: i64,
    threshold: f64,
    mode: Option<String>,
    visual_threshold: Option<f64>,
    semantic_threshold: Option<f64>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mode = crate::clustering::L2Mode::parse(mode.as_deref().unwrap_or("text"));
    let visual_threshold = visual_threshold
        .unwrap_or(crate::clustering::DEFAULT_VISUAL_THRESHOLD);
    // 语义档（BGE）的阈值是**另一个量纲**，绝不可沿用 CLIP 的 0.75。
    let semantic_threshold = semantic_threshold
        .unwrap_or(crate::clustering::DEFAULT_SEMANTIC_THRESHOLD);
    // 文本模式的阈值始终按源保存（后台同步要沿用同一个值）
    state
        .db
        .set_source_threshold(source_id, threshold)
        .map_err(|e| e.to_string())?;
    state
        .db
        .set_source_l2_mode(source_id, mode.as_str())
        .map_err(|e| e.to_string())?;
    if mode.uses_cosine() {
        state
            .db
            .set_source_visual_threshold(source_id, visual_threshold)
            .map_err(|e| e.to_string())?;
    }
    if mode == crate::clustering::L2Mode::Semantic {
        state
            .db
            .set_source_semantic_threshold(source_id, semantic_threshold)
            .map_err(|e| e.to_string())?;
    }
    crate::clustering::recluster_l2_with_mode(
        &state.db,
        source_id,
        mode,
        threshold,
        visual_threshold,
        semantic_threshold,
    )
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

#[tauri::command(async)]
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
    let dir = crate::paths::data_root(&app);
    dir.join("ai-image-sorter.db")
        .to_string_lossy()
        .to_string()
}

// ── 库恢复（第 29 轮）──────────────────────────────────────────
//
// 「改了名 → 手工分拣结果消失」这件事，**扫描期**只能救回**还没被删**的
// 那部分（改名不丢分拣）。已经被删掉的那部分，如果**老库文件还在**，就能
// 从老库里按内容身份找回来 —— 这就是「库恢复」。
//
// 用户侧的现实：装了安装版、又跑了便携版，两边各一个库；或者改了应用数据
// 目录。老的那个库就成了「没人用的副本」，里面还留着分拣结果。

/// 列出**可能**是旧副本的库文件（当前库目录的兄弟文件 + 安装版的同名库）。
#[tauri::command]
pub fn list_library_candidates(app: tauri::AppHandle) -> Vec<String> {
    let current = crate::paths::data_root(&app)
        .join("ai-image-sorter.db")
        .to_string_lossy()
        .to_string();
    let mut out = crate::reconcile::candidate_libraries(&current);
    // 安装版的数据目录（便携模式下也可能有 —— 换发行方式时留下的）。
    if let Ok(dir) = app.path().app_data_dir() {
        let installed = dir.join("ai-image-sorter.db");
        let p = installed.to_string_lossy().to_string();
        if installed.is_file() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// 预览「能从这个旧库里找回多少」。
#[tauri::command]
pub fn preview_library_recovery(
    path: String,
    state: State<'_, AppState>,
) -> Result<crate::reconcile::Preview, String> {
    crate::reconcile::preview(&state.db, &path)
}

/// 执行恢复。
#[tauri::command]
pub fn apply_library_recovery(
    path: String,
    state: State<'_, AppState>,
) -> Result<crate::reconcile::Outcome, String> {
    crate::reconcile::apply(&state.db, &path)
}

/// 用户确认「这张图确实被删了」→ 把那条**身份无法确认**的旧记录删掉。
///
/// 这是「绝不静默删除」的另一半：先保留、再让用户决定。
#[tauri::command]
pub fn discard_unconfirmed_image(
    image_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.db.delete_image_row(image_id).map_err(|e| e.to_string())
}

/// 身份无法确认、被保留待确认的旧记录（第 29 轮）。
///
/// 判据：文件已经不在原路径上，**而且**这条记录带用户状态（评分 / 标签 /
/// 手动分组）。前者是「图没了」，后者是「不能悄悄扔掉」—— 两个都成立才
/// 进这份名单。
#[derive(Debug, Clone, Serialize)]
pub struct UnconfirmedImageDto {
    pub id: i64,
    pub abs_path: String,
    pub filename: String,
    pub reason: String,
}

#[tauri::command]
pub fn list_unconfirmed_images(
    state: State<'_, AppState>,
) -> Result<Vec<UnconfirmedImageDto>, String> {
    let sources = state.db.list_sources().map_err(|e| e.to_string())?;
    let mut out: Vec<UnconfirmedImageDto> = Vec::new();
    for src in sources {
        let rows = state
            .db
            .list_source_identity_rows(src.id)
            .map_err(|e| e.to_string())?;
        let user_state = state
            .db
            .images_with_user_state(src.id)
            .map_err(|e| e.to_string())?;
        for row in rows {
            if !user_state.contains(&row.id) {
                continue;
            }
            if PathBuf::from(&row.abs_path).is_file() {
                continue;
            }
            let filename = row
                .abs_path
                .rsplit('/')
                .next()
                .unwrap_or(&row.abs_path)
                .to_string();
            out.push(UnconfirmedImageDto {
                id: row.id,
                abs_path: row.abs_path.clone(),
                filename,
                reason: "文件已不在原路径，且无法按内容确认身份 —— 记录已保留".to_string(),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupThumbDto {
    pub group_key: String,
    pub thumb_paths: Vec<String>,
}

#[tauri::command(async)]
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
            db.list_source_identity_rows(1).map(|v| v.len()).unwrap_or(0) + broken,
            empty_ck,
            no_key,
            with_model,
            matched,
            assigned,
            broken,
        );

        let conn = db.0.lock().unwrap();
        let groups = {
            let mut stmt = conn
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

    /// 把文件的修改时间往回拨 —— 扫描有 10 秒的**稳定性窗口**（正在写的
    /// 文件先不碰），刚写出来的文件会被推迟一轮。
    fn age_file(path: &std::path::Path) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        file.set_times(std::fs::FileTimes::new().set_modified(past))
            .unwrap();
    }

    /// 第 29 轮「改名不丢分拣」：**改名不换行**。
    ///
    /// 场景：两张图入库 → 用户在应用之外改名 → 再扫一次。
    /// 期望：**image_id 不变**（评分 / 标签 / 手动分组因此全部保住），
    /// 且库里不再有「文件已不在」的行。
    #[test]
    fn rename_keeps_the_row_and_its_state() {
        let dir = std::env::temp_dir().join(format!("ais-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 两个**大小不同**的文件 —— 尺寸是内容主键的一部分，
        // 足以让「这两张是不同的图」这件事被认出来。
        let a = dir.join("ComfyUI_00001_.png");
        let b = dir.join("ComfyUI_00002_.png");
        std::fs::write(&a, vec![7u8; 100]).unwrap();
        std::fs::write(&b, vec![9u8; 200]).unwrap();
        age_file(&a);
        age_file(&b);

        let db_path = dir.join("test.db");
        let db = crate::db::open(&db_path).unwrap();
        let source_id = db
            .upsert_source(&normalize_path(&dir), "local", None)
            .unwrap();
        let norm = normalize_path(&dir);
        let (files, _) = comfy_finder::list_images_with_newest(&dir);
        let first = scan_source(&db, source_id, &norm, &files).unwrap();
        assert_eq!(first.added, 2, "首次扫描应入库两张");
        assert_eq!(first.renamed.len(), 0, "首次扫描不该有改名");

        // 给其中一张记一个「用户状态」—— 评分。
        let ids: Vec<i64> = db
            .list_source_identity_rows(source_id)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids.len(), 2);
        db.set_score(ids[0], 0.75).unwrap();

        // 改名（只改文件名，内容不变）。
        let renamed_a = dir.join("20260727_00001_.png");
        std::fs::rename(&a, &renamed_a).unwrap();

        let (files2, _) = comfy_finder::list_images_with_newest(&dir);
        let second = scan_source(&db, source_id, &norm, &files2).unwrap();
        assert_eq!(second.renamed.len(), 1, "应识别出一次改名");
        assert_eq!(second.added, 0, "改名不该被当成新图");
        assert_eq!(second.unconfirmed.len(), 0, "改名认出来了，不该有待确认");

        // 行数不变、image_id 不变、评分还在。
        let rows = db.list_source_identity_rows(source_id).unwrap();
        assert_eq!(rows.len(), 2, "库里的行数不该变（换行 = 丢数据）");
        assert!(rows.iter().any(|r| r.abs_path.contains("20260727_00001_")));
        assert_eq!(
            db.score_of(ids[0]).unwrap(),
            Some(0.75),
            "改名的图必须保住它的评分"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 第 29 轮：**身份认不出来时绝不静默删除**。
    ///
    /// 场景：一张带评分的图，它的文件被删了（不是改名 —— 认不出来）。
    /// 期望：这一行**被保留**并进「待确认」名单，绝不悄悄消失。
    #[test]
    fn unidentifiable_missing_row_is_preserved_not_deleted() {
        let dir = std::env::temp_dir().join(format!("ais-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("ComfyUI_00001_.png");
        std::fs::write(&a, vec![7u8; 100]).unwrap();
        age_file(&a);

        let db_path = dir.join("test.db");
        let db = crate::db::open(&db_path).unwrap();
        let source_id = db.upsert_source(&normalize_path(&dir), "local", None).unwrap();
        let norm = normalize_path(&dir);
        let (files, _) = comfy_finder::list_images_with_newest(&dir);
        scan_source(&db, source_id, &norm, &files).unwrap();
        let id = db.list_source_identity_rows(source_id).unwrap()[0].id;
        db.set_score(id, 0.5).unwrap();

        // 文件没了，且没有新文件来认领它。
        std::fs::remove_file(&a).unwrap();
        let (files2, _) = comfy_finder::list_images_with_newest(&dir);
        let second = scan_source(&db, source_id, &norm, &files2).unwrap();
        assert_eq!(second.unconfirmed.len(), 1, "认不出来就该进待确认名单");
        let rows = db.list_source_identity_rows(source_id).unwrap();
        assert_eq!(rows.len(), 1, "带用户状态的行绝不能被静默删除");
        assert_eq!(db.score_of(id).unwrap(), Some(0.5), "评分必须还在");
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

// ---------------------------------------------------------------------------
// 视觉签名（阶段六 Round 1）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SignatureProgressDto {
    pub slot: String,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignatureResultDto {
    pub computed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total: usize,
    pub model_id: String,
    pub dim: usize,
    pub cancelled: bool,
}

/// 组内「疑似错分」的图片。
///
/// 用户最痛的描述是「大组里那两三张被错误识别出来的」。这个命令把那张图
/// 找出来：它在组里**没有任何视觉上够格的伙伴**（与组内其它成员的最大余弦
/// 低于门槛），并且给出它**最像**的那个别的组。
#[derive(Debug, Clone, Serialize)]
pub struct OutlierInfo {
    pub image_id: i64,
    /// 它在组内与其它成员的最大余弦（越低越像「不属于这里」）。
    pub max_similarity: f64,
    /// 最像的别的组（没有向量时为 None）。
    pub best_group_key: Option<String>,
    pub best_score: f64,
}

/// 找出组内疑似错分的图片。
#[tauri::command]
pub fn group_outliers(
    group_key: String,
    threshold: Option<f64>,
    state: State<'_, AppState>,
) -> Result<Vec<OutlierInfo>, String> {
    // 门槛用「视觉上算不算同一类」的尺度：0.75 与语义聚类的默认阈值同源。
    let threshold = threshold.unwrap_or(crate::clustering::DEFAULT_VISUAL_THRESHOLD);
    let db = &state.db;
    let members = db.group_member_ids(&group_key).map_err(|e| e.to_string())?;
    if members.len() < 2 {
        return Ok(Vec::new());
    }
    // 组内成员的向量。
    //
    // 槽位跟着该组的 L2 档位走（口径收口在 `L2Mode::signature_spec`）：
    // 语义档用文本槽的向量，视觉档用视觉槽的向量。读错槽就是**错空间**
    // （两塔都是 512 维，肉眼查不出来）。
    let Some(source_id) = db.group_source_id(&group_key).map_err(|e| e.to_string())? else {
        return Ok(Vec::new());
    };
    let mode = crate::clustering::L2Mode::parse(
        &db.get_source_l2_mode(source_id)
            .unwrap_or_else(|_| "text".into()),
    );
    let (slot, model_id) = mode
        .signature_spec()
        .unwrap_or(("vision", crate::vision::VISION_MODEL_ID));
    let mut vectors: HashMap<i64, Vec<f32>> = HashMap::new();
    for (id, vec) in db
        .signatures_for_slot(slot, model_id)
        .map_err(|e| e.to_string())?
    {
        vectors.insert(id, vec);
    }
    let member_ids: Vec<i64> = members
        .iter()
        .copied()
        .filter(|id| vectors.contains_key(id))
        .collect();
    if member_ids.len() < 2 {
        return Ok(Vec::new());
    }
    let dot = |a: i64, b: i64| -> f32 {
        let (Some(x), Some(y)) = (vectors.get(&a), vectors.get(&b)) else {
            return f32::NEG_INFINITY;
        };
        let mut sum = 0f32;
        for i in 0..x.len().min(y.len()) {
            sum += x[i] * y[i];
        }
        sum
    };
    // 每个成员与组内其它成员的最大相似度
    let mut max_sim: HashMap<i64, f32> = HashMap::new();
    for a in &member_ids {
        let mut best = f32::NEG_INFINITY;
        for b in &member_ids {
            if a == b {
                continue;
            }
            best = best.max(dot(*a, *b));
        }
        max_sim.insert(*a, best);
    }
    let outliers: Vec<i64> = member_ids
        .iter()
        .copied()
        .filter(|id| (max_sim.get(id).copied().unwrap_or(1.0) as f64) < threshold)
        .collect();
    if outliers.is_empty() {
        return Ok(Vec::new());
    }
    // 别的组（同一来源，排除自己）作为候选
    let mut groups: Vec<(String, Vec<i64>)> = Vec::new();
    for (key, _count, _prompt, ids) in db.group_members_for_source(source_id).unwrap_or_default() {
        if key != group_key {
            groups.push((key, ids));
        }
    }
    let mut out = Vec::new();
    for id in outliers {
        let mut best_key: Option<String> = None;
        let mut best_score = 0f32;
        for (key, ids) in &groups {
            let mut best = f32::NEG_INFINITY;
            for mid in ids {
                if !vectors.contains_key(mid) {
                    continue;
                }
                best = best.max(dot(id, *mid));
            }
            if best.is_finite() && best > best_score {
                best_score = best;
                best_key = Some(key.clone());
            }
        }
        out.push(OutlierInfo {
            image_id: id,
            max_similarity: max_sim.get(&id).copied().unwrap_or(0.0) as f64,
            best_group_key: best_key,
            best_score: best_score as f64,
        });
    }
    out.sort_by(|a, b| {
        a.max_similarity
            .partial_cmp(&b.max_similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// 「可能相关」推荐：给定选中的图，哪个组最可能该收留它们。
///
/// 这是阶段六最直接解痛的一步：用户在大组里发现两三张图被错误识别，
/// 想把它们挪走，但目标组在一长串里看不出来。这个命令把每个候选组按
/// 「与选中图的最大相似度」排好序 —— 有视觉签名时用视觉余弦，
/// 没有时退回 prompt 的词 Jaccard（永远可用）。
#[tauri::command]
pub fn suggest_move_targets(
    source_id: i64,
    image_ids: Vec<i64>,
    current_l2: String,
    state: State<'_, AppState>,
) -> Result<Vec<crate::suggest::Suggestion>, String> {
    let db = &state.db;
    let groups = db.group_members_for_source(source_id).map_err(|e| e.to_string())?;
    let mut members: Vec<crate::suggest::GroupMembers> = Vec::new();
    for (key, count, prompt, ids) in groups {
        if key == current_l2 {
            continue;
        }
        members.push(crate::suggest::GroupMembers {
            group_key: key,
            count,
            prompt_pos: prompt,
            members: ids,
        });
    }
    if members.is_empty() {
        return Ok(Vec::new());
    }
    // 基准跟着该源的 L2 档位走：视觉档用视觉余弦，语义档用语义余弦，
    // 文本档没有向量 → 退回词 Jaccard（永远可用）。
    //
    // 口径收口在 `L2Mode::signature_spec()`：槽位与模型必须配套，
    // 读错槽就是**错空间**（两塔都是 512 维，肉眼查不出来）。
    let mode = crate::clustering::L2Mode::parse(
        &db.get_source_l2_mode(source_id).unwrap_or_else(|_| "text".into()),
    );
    let spec = mode.signature_spec();
    let vector_basis = match spec {
        Some(("vision", _)) => "vision",
        Some(("text", _)) => "semantic",
        _ => "text",
    };
    // 签名（有就用向量）
    let mut vectors: HashMap<i64, Vec<f32>> = HashMap::new();
    if let Some((slot, model_id)) = spec {
        for (id, vec) in db
            .signatures_for(source_id, slot, model_id)
            .map_err(|e| e.to_string())?
        {
            vectors.insert(id, vec);
        }
    }
    // 文本退路：prompt 的词集合
    let conn = db.0.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT id, prompt_pos FROM images WHERE source_id=?1 AND prompt_pos != ''")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![source_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut tokens: crate::suggest::TokenSets = HashMap::new();
    {
        let rows = rows;
        for row in rows {
            let (id, prompt) = row.map_err(|e| e.to_string())?;
            tokens.insert(
                id,
                crate::clustering::tokenize(&prompt).into_iter().collect(),
            );
        }
    }
    drop(stmt);
    drop(conn);

    Ok(crate::suggest::score_groups(
        &members,
        &image_ids,
        &vectors,
        &tokens,
        vector_basis,
    ))
}

#[derive(Debug, Clone, Serialize)]
pub struct VisionStatusDto {
    /// 权重是否就位。不就位时前端把「识图」入口标成不可用，而不是让用户
    /// 点下去才报错。
    pub available: bool,
    pub model_id: String,
    pub model_path: String,
}

/// 「识图」是否可用（模型权重是否就位）。
#[tauri::command]
pub fn vision_status(state: State<'_, AppState>) -> Result<VisionStatusDto, String> {
    let path = state.vision.model_path().to_path_buf();
    Ok(VisionStatusDto {
        available: path.is_file(),
        model_id: crate::vision::VISION_MODEL_ID.to_string(),
        model_path: path.to_string_lossy().to_string(),
    })
}

/// 取消正在跑的签名批处理。
#[tauri::command]
pub fn cancel_signatures(state: State<'_, AppState>) -> Result<bool, String> {
    let guard = state.signature_cancel.lock().map_err(|_| "取消标志锁已中毒")?;
    match guard.as_ref() {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// 文本塔（prompt 语义）的状态。
#[derive(Debug, Clone, Serialize)]
pub struct TextStatusDto {
    /// 权重与分词器是否**都在**（缺任一份 → 整个塔不可用）。
    pub available: bool,
    pub model_id: String,
    pub model_path: String,
    pub tokenizer_path: String,
    /// 分词器词表大小（可读时才有值）。
    pub vocab_size: Option<usize>,
}

/// 「文本塔」是否可用。
#[tauri::command]
pub fn text_status(state: State<'_, AppState>) -> Result<TextStatusDto, String> {
    let available = state.text.available();
    let vocab_size = if available {
        state
            .text
            .with_engine(|engine| engine.tokenizer().vocab_size())
            .ok()
    } else {
        None
    };
    Ok(TextStatusDto {
        available,
        model_id: crate::text::TEXT_MODEL_ID.to_string(),
        model_path: state.text.model_path().to_string_lossy().to_string(),
        tokenizer_path: state.text.tokenizer_path().to_string_lossy().to_string(),
        vocab_size,
    })
}

/// 文本塔覆盖率（该源有多少条 prompt 已经算出向量）。
#[derive(Debug, Clone, Serialize)]
pub struct TextCoverageDto {
    /// 有非空 prompt 的图数（分母）。
    pub total: usize,
    /// 已算出文本签名的图数（分子）。
    pub computed: usize,
    /// 向量维度（已算过才有值）。
    pub dim: Option<usize>,
}

/// 该源的文本塔覆盖率。
#[tauri::command]
pub fn text_coverage(
    source_id: i64,
    state: State<'_, AppState>,
) -> Result<TextCoverageDto, String> {
    let total = state
        .db
        .count_images_with_prompts(source_id)
        .map_err(|e| e.to_string())?;
    let computed = state
        .db
        .signature_image_ids(source_id, "text", crate::text::TEXT_MODEL_ID)
        .map_err(|e| e.to_string())?
        .len();
    let dim = state
        .db
        .signatures_for(source_id, "text", crate::text::TEXT_MODEL_ID)
        .map_err(|e| e.to_string())?
        .first()
        .map(|(_, v)| v.len());
    Ok(TextCoverageDto {
        total,
        computed,
        dim,
    })
}

/// 取消正在跑的文本签名批处理。
#[tauri::command]
pub fn cancel_text_signatures(state: State<'_, AppState>) -> Result<bool, String> {
    let guard = state
        .text_signature_cancel
        .lock()
        .map_err(|_| "取消标志锁已中毒")?;
    match guard.as_ref() {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// 给一个源的所有 prompt 算文本签名（BGE）。
///
/// 与视觉塔的差别：
/// * 输入是 **prompt 文本**，不读图 —— 所以快得多（1000 条 prompt 约十几秒）；
/// * 批大小恒为 8（`text::TEXT_BATCH`）—— int8 量化权重对批大小敏感，
///   与实验室 `embed.py` 同口径，否则向量闸门失去判别力；
/// * 写进 `slot='text'`、`model_id='bge-small-zh-q'` —— **错空间**的坑就在
///   这里（视觉塔也是 512 维，肉眼查不出来）。
///
/// 逐批回传进度（`text-signature-progress` 事件），可取消。
#[tauri::command]
pub async fn compute_text_signatures(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: i64,
    force: Option<bool>,
) -> Result<SignatureResultDto, String> {
    if !state.text.available() {
        return Err("未找到文本塔资源（需要 text_bge.onnx 与 tokenizer_bge.json，请运行 scripts\\download_models.ps1）".to_string());
    }
    // 同一时刻只允许一批（文本塔与视觉塔各一把锁，互不打断）。
    {
        let guard = state
            .text_signature_cancel
            .lock()
            .map_err(|_| "取消标志锁已中毒")?;
        if guard.is_some() {
            return Err("已有文本签名任务在运行".to_string());
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut guard = state
            .text_signature_cancel
            .lock()
            .map_err(|_| "取消标志锁已中毒")?;
        *guard = Some(Arc::clone(&cancel));
    }
    let db = Arc::clone(&state.db);
    let text = Arc::clone(&state.text);
    let force = force.unwrap_or(false);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let images = db
            .list_images_for_prompts(source_id)
            .map_err(|e| e.to_string())?;
        let total = images.len();
        let already = if force {
            std::collections::HashSet::new()
        } else {
            db.signature_image_ids(source_id, "text", crate::text::TEXT_MODEL_ID)
                .map_err(|e| e.to_string())?
        };
        let todo: Vec<(i64, String)> = images
            .into_iter()
            .filter(|(id, _)| !already.contains(id))
            .collect();
        let skipped = total.saturating_sub(todo.len());
        let mut failed = 0usize;
        let mut computed = 0usize;
        let mut dim = 0usize;
        if !todo.is_empty() {
            let base = crate::progress::ProgressBase::new();
            let mut progress = |done: usize, _total: usize| {
                let _ = app.emit(
                    "text-signature-progress",
                    SignatureProgressDto {
                        slot: "text".to_string(),
                        done: base.global(done),
                        total: todo.len(),
                    },
                );
            };
            for chunk in todo.chunks(64) {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let computed_chunk = text.with_engine(|engine| {
                    engine.embed_prompts(chunk, &mut progress, &|| cancel.load(Ordering::SeqCst))
                });
                match computed_chunk {
                    Ok(Ok((rows, chunk_failed))) => {
                        failed += chunk_failed;
                        computed += rows.len();
                        if let Some(first) = rows.first() {
                            dim = first.1.len();
                        }
                        db.upsert_signatures(
                            "text",
                            crate::text::TEXT_MODEL_ID,
                            &rows
                                .iter()
                                .map(|(id, vec)| (*id, vec.as_slice()))
                                .collect::<Vec<_>>(),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    Ok(Err(e)) => {
                        log::warn!("文本签名批次失败：{e}");
                        failed += chunk.len();
                    }
                    Err(e) => {
                        log::warn!("文本塔不可用：{e}");
                        failed += chunk.len();
                    }
                }
                base.advance(chunk.len());
            }
        }
        let cancelled = cancel.load(Ordering::SeqCst);
        Ok::<SignatureResultDto, String>(SignatureResultDto {
            computed,
            skipped,
            failed,
            total,
            model_id: crate::text::TEXT_MODEL_ID.to_string(),
            dim,
            cancelled,
        })
    })
    .await
    .map_err(|e| format!("文本签名任务异常：{e}"))??;
    {
        let mut guard = state
            .text_signature_cancel
            .lock()
            .map_err(|_| "取消标志锁已中毒")?;
        *guard = None;
    }
    Ok(result)
}

/// 能力标记串：签名**只按需计算**（第 30 轮）。
///
/// 与它成对的是一条**否定**检查：发行版探针要确认「后台补算」那段代码已经
/// 不在二进制里了（见 `scripts/verify-release-binary.ps1` 的 `Absent`）。
///
/// 为什么值得专门证明：这段代码**曾经**存在，而且它的存在**看不出来** ——
/// 它不会让 exe 变大多少，也不会报错，只是每 8 秒偷偷烧一次 CPU。删掉它
/// 同样看不出来（`opt-level="s" + lto=true` 会把整段剔干净）。所以「没有」
/// 也得能证明。
pub const CAP_SIGNATURE_EXPLICIT: &str = "ais.cap.signature.explicit.v1\0";

/// 该源的**签名覆盖率**（第 30 轮）。
///
/// 同步循环不再偷偷补算签名（见 `run_sync` 里的说明），所以「有多少图还没
/// 向量」必须由界面**如实说出来** —— 缺向量的图保持原组、暂不参与聚类
/// （纪律 #6：不做无声的搬动）。
#[derive(Debug, Clone, Serialize)]
pub struct SignatureCoverageDto {
    /// 当前 L2 档位（text / visual / semantic）。
    pub mode: String,
    /// 该档要读哪个槽位（text 档为 None —— 它用词 Jaccard，不需要向量）。
    pub slot: Option<String>,
    pub model_id: Option<String>,
    /// 分母：会参与聚类的图数。
    pub total: usize,
    /// 分子：已经有向量的图数。
    pub computed: usize,
}

#[tauri::command]
pub fn l2_signature_coverage(
    source_id: i64,
    state: State<'_, AppState>,
) -> Result<SignatureCoverageDto, String> {
    let mode = crate::clustering::L2Mode::parse(
        &state
            .db
            .get_source_l2_mode(source_id)
            .unwrap_or_else(|_| "text".into()),
    );
    let spec = mode.signature_spec();
    let (total, computed) = match spec {
        None => {
            // 文本档用词 Jaccard，不需要向量 → 没有「缺签名」这回事。
            (0, 0)
        }
        Some((slot, model_id)) => {
            let computed = state
                .db
                .signature_image_ids(source_id, slot, model_id)
                .map_err(|e| e.to_string())?
                .len();
            let total = if slot == "text" {
                state
                    .db
                    .count_images_with_prompts(source_id)
                    .map_err(|e| e.to_string())?
            } else {
                state
                    .db
                    .count_source_images(source_id)
                    .map_err(|e| e.to_string())?
            };
            (total, computed)
        }
    };
    Ok(SignatureCoverageDto {
        mode: mode.as_str().to_string(),
        slot: spec.map(|(slot, _)| slot.to_string()),
        model_id: spec.map(|(_, model_id)| model_id.to_string()),
        total,
        computed,
    })
}

/// 一个模型塔的运行时状态。
#[derive(Debug, Clone, Serialize)]
pub struct TowerCapabilityDto {
    pub id: &'static str,
    pub label: &'static str,
    pub model_id: String,
    /// 权重是否就位 —— 就位 = 这次运行里这个塔真的能用（不是「代码在」）。
    pub available: bool,
}

/// 构建能力自述。
#[derive(Debug, Clone, Serialize)]
pub struct AppCapabilitiesDto {
    /// 应用版本（来自 `tauri.conf.json`）。
    pub version: String,
    /// 构建时刻（编译期烧进二进制的 epoch 秒）—— 用来认出「这是哪一份 exe」。
    pub build_ts: String,
    /// 构建时的 git 短 SHA（无 git 时为 `nogit`）。
    pub build_id: String,
    /// 本次构建具备的能力（标记串住在实现模块里，见 `capabilities.rs`）。
    pub capabilities: Vec<crate::capabilities::Capability>,
    /// L2 聚类档位（运行时真值）。
    pub l2_modes: Vec<String>,
    /// 各塔的就位状态。
    pub towers: Vec<TowerCapabilityDto>,
}

/// 这个 exe 里到底有什么。
///
/// 为什么要有这个命令：发行版 EXE 是**用户主动审查的产物**。用户要能
/// 打开应用就看到「这一份带哪些能力」，而不是靠文件大小 / 时间戳去猜
/// （阶段七轮 2 的 exe 与上一版逐字节等价，肉眼完全看不出来）。
///
/// 标记串的纪律见 `capabilities.rs`：它们住在**实现模块**里，所以这份
/// 清单不可能声称一个没实现的能力。
#[tauri::command]
pub fn app_capabilities(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppCapabilitiesDto, String> {
    let version = app.package_info().version.to_string();
    let build_ts = match env!("AI_SORTER_BUILD_TS").parse::<i64>() {
        Ok(ts) => chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| env!("AI_SORTER_BUILD_TS").to_string()),
        Err(_) => env!("AI_SORTER_BUILD_TS").to_string(),
    };
    let towers = vec![TowerCapabilityDto {
        id: "tower.vision",
        label: "视觉塔（CLIP ViT-B/32）",
        model_id: crate::vision::VISION_MODEL_ID.to_string(),
        available: state.vision.model_path().is_file(),
    }];
    Ok(AppCapabilitiesDto {
        version,
        build_ts,
        build_id: env!("AI_SORTER_BUILD_ID").to_string(),
        capabilities: crate::capabilities::capabilities(),
        l2_modes: crate::clustering::L2Mode::all()
            .iter()
            .map(|m| m.as_str().to_string())
            .collect(),
        towers,
    })
}

/// 给一个源的所有图片算视觉签名。
///
/// 逐批处理并回传进度（`signature-progress` 事件）：1000 张在 CPU 上约 5 分钟，
/// 必须让用户看得见进度、也必须能取消。已算过的图默认跳过（`force` 可重算）。
#[tauri::command]
pub async fn compute_signatures(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: i64,
    force: Option<bool>,
) -> Result<SignatureResultDto, String> {
    if !state.vision.model_path().is_file() {
        return Err("未找到视觉模型权重（请运行 scripts\\download_models.ps1）".to_string());
    }
    // 同一时刻只允许一批
    {
        let guard = state.signature_cancel.lock().map_err(|_| "取消标志锁已中毒")?;
        if guard.is_some() {
            return Err("已有签名任务在运行".to_string());
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut guard = state.signature_cancel.lock().map_err(|_| "取消标志锁已中毒")?;
        *guard = Some(Arc::clone(&cancel));
    }
    let db = Arc::clone(&state.db);
    let vision = Arc::clone(&state.vision);
    let force = force.unwrap_or(false);
    let result = tauri::async_runtime::spawn_blocking(move || {
        let images = db.list_images_for_signatures(source_id).map_err(|e| e.to_string())?;
        let total = images.len();
        let already = if force {
            std::collections::HashSet::new()
        } else {
            db.signature_image_ids(source_id, "vision", crate::vision::VISION_MODEL_ID)
                .map_err(|e| e.to_string())?
        };
        let todo: Vec<(i64, PathBuf)> = images
            .into_iter()
            .filter(|(id, path)| {
                !already.contains(id) && std::path::Path::new(path).is_file()
            })
            .map(|(id, path)| (id, PathBuf::from(path)))
            .collect();
        let skipped = total.saturating_sub(todo.len());
        let mut failed = 0usize;
        let mut computed = 0usize;
        let mut dim = 0usize;
        if !todo.is_empty() {
            // 跨块累加的进度基址。`embed_batch` 上报的是**块内**计数
            // （每 8 张一批，上限 = 块长 ≤ 64），直接当全局计数发出会让
            // 进度条在每块开头**重置** —— 实测症状是 1%↔7% 反复横跳，
            // 而计算本身完全正常。详见 `progress.rs`。
            let base = crate::progress::ProgressBase::new();
            let mut progress = |done: usize, _total: usize| {
                let _ = app.emit(
                    "signature-progress",
                    SignatureProgressDto {
                        slot: "vision".to_string(),
                        done: base.global(done),
                        total: todo.len(),
                    },
                );
            };
            // 分块跑：单张图读失败不该让整批失败（embed_batch 内部跳过并计数）。
            for chunk in todo.chunks(64) {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                match vision.embed_batch(chunk, &mut progress, &|| cancel.load(Ordering::SeqCst)) {
                    Ok((rows, chunk_failed)) => {
                        failed += chunk_failed;
                        computed += rows.len();
                        if let Some(first) = rows.first() {
                            dim = first.1.len();
                        }
                        db.upsert_signatures(
                            "vision",
                            crate::vision::VISION_MODEL_ID,
                            &rows
                                .iter()
                                .map(|(id, vec)| (*id, vec.as_slice()))
                                .collect::<Vec<_>>(),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    Err(e) => {
                        log::warn!("签名批次失败：{e}");
                        failed += chunk.len();
                    }
                }
                base.advance(chunk.len());
            }
        }
        let cancelled = cancel.load(Ordering::SeqCst);
        Ok::<SignatureResultDto, String>(SignatureResultDto {
            computed,
            skipped,
            failed,
            total,
            model_id: crate::vision::VISION_MODEL_ID.to_string(),
            dim,
            cancelled,
        })
    })
    .await
    .map_err(|e| format!("签名任务异常：{e}"))??;
    {
        let mut guard = state.signature_cancel.lock().map_err(|_| "取消标志锁已中毒")?;
        *guard = None;
    }
    Ok(result)
}

