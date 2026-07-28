use crate::db::{ImageInsert, ImageRow, LabelRow, SourceRow};
use crate::grouper::compute_group_keys;
use crate::metadata::{parse_file, ImageMeta};
use crate::{archive, comfy_finder, db::GroupInfo, scoring};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{Manager, State};

pub struct AppState {
    pub db: crate::db::Db,
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
}

#[tauri::command]
pub fn add_source_and_scan(
    args: AddSourceArgs,
    state: State<'_, AppState>,
) -> Result<ScanResult, String> {
    let source_id = state
        .db
        .upsert_source(&args.path, &args.kind, args.alias.as_deref())
        .map_err(|e| e.to_string())?;

    // Idempotent rescan: keep rows for files still on disk (their id and
    // thus their scores/labels survive), insert brand-new files only, and
    // delete rows whose abs_path no longer exists (cascades to score/label).
    let existing = state
        .db
        .list_source_image_paths(source_id)
        .map_err(|e| e.to_string())?;
    let mut existing_map: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for (id, p) in &existing {
        existing_map.insert(p.clone(), *id);
    }

    let files = comfy_finder::list_images_in(&PathBuf::from(&args.path));
    let mut groups_count = 0usize;
    let mut scanned = 0usize;
    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut on_disk: std::collections::HashSet<String> = std::collections::HashSet::new();

    for f in &files {
        let abs = f.to_string_lossy().to_string();
        on_disk.insert(abs.clone());
        if existing_map.contains_key(&abs) {
            // File already scanned; metadata is immutable, keep row,
            // still count it for the visible total.
            scanned += 1;
            // L3 group key already stored; recompute only for new rows.
            continue;
        }

        let meta = parse_file(f).unwrap_or_else(|_| ImageMeta::default());
        let keys = compute_group_keys(&meta, &args.path);
        if seen_keys.insert(keys.l3.clone()) {
            groups_count += 1;
        }
        let (w, h) = image_dims(f).unwrap_or((0, 0));
        let size = std::fs::metadata(f).map(|m| m.len() as i64).unwrap_or(0);
        let seed = meta.samplers.first().map(|s| s.seed).unwrap_or(0);
        let steps = meta.samplers.first().map(|s| s.steps).unwrap_or(0);
        let cfg = meta.samplers.first().map(|s| s.cfg).unwrap_or(0.0);
        let sha = archive::sha256_of(f).unwrap_or_default();
        let rel = f.to_string_lossy().to_string();

        let row = ImageInsert {
            source_id,
            rel_path: rel,
            abs_path: abs.clone(),
            filename: f
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            size,
            width: w as i64,
            height: h as i64,
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
            sha256: sha,
            meta_ok: meta.raw_ok,
            source_kind: format!("{:?}", meta.source_kind).to_lowercase(),
        };
        state.db.insert_image(&row).map_err(|e| e.to_string())?;
        scanned += 1;
    }

    // Remove rows for files that vanished from disk.
    state
        .db
        .delete_missing_source_images(source_id, &on_disk)
        .map_err(|e| e.to_string())?;

    // Re-cluster L2 within this source so the similarity groups reflect
    // the now-complete member set. Stable prompt-based keys keep existing
    // clusters at the same id; only genuinely new buckets appear.
    crate::clustering::recluster_l2(&state.db, source_id).map_err(|e| e.to_string())?;

    Ok(ScanResult {
        source_id,
        scanned,
        groups: groups_count,
    })
}

fn image_dims(path: &std::path::Path) -> Option<(u32, u32)> {
    let reader = image::io::Reader::open(path).ok()?;
    let reader = reader.with_guessed_format().ok()?;
    let dims = reader.into_dimensions().ok()?;
    Some(dims)
}

#[tauri::command]
pub fn list_sources(state: State<'_, AppState>) -> Result<Vec<SourceRow>, String> {
    state.db.list_sources().map_err(|e| e.to_string())
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
    let conn = state.db.0.lock().unwrap();
    let cur: Option<f64> = conn
        .query_row(
            "SELECT internal_score FROM scores WHERE image_id=?1",
            rusqlite::params![image_id],
            |r| r.get(0),
        )
        .ok();
    drop(conn);
    let next = scoring::apply_swipe(cur, &gesture);
    state
        .db
        .set_score(image_id, next, "swipe")
        .map_err(|e| e.to_string())?;
    Ok(next)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArenaArgs {
    pub group_key: String,
    pub left: i64,
    pub right: i64,
    pub winner_is_left: bool,
}

#[tauri::command]
pub fn arena_vote(args: ArenaArgs, state: State<'_, AppState>) -> Result<(f64, f64), String> {
    let conn = state.db.0.lock().unwrap();
    let l: f64 = conn
        .query_row(
            "SELECT internal_score FROM scores WHERE image_id=?1",
            rusqlite::params![args.left],
            |r| r.get(0),
        )
        .unwrap_or(scoring::BASE_SCORE);
    let r: f64 = conn
        .query_row(
            "SELECT internal_score FROM scores WHERE image_id=?1",
            rusqlite::params![args.right],
            |r| r.get(0),
        )
        .unwrap_or(scoring::BASE_SCORE);
    drop(conn);
    let (nl, nr) = scoring::apply_arena(l, r, args.winner_is_left);
    state
        .db
        .set_score(args.left, nl, "arena")
        .map_err(|e| e.to_string())?;
    state
        .db
        .set_score(args.right, nr, "arena")
        .map_err(|e| e.to_string())?;
    state
        .db
        .add_compare_pair(
            &args.group_key,
            args.left,
            args.right,
            if args.winner_is_left { args.left } else { args.right },
            (nl - l).abs(),
        )
        .map_err(|e| e.to_string())?;
    Ok((nl, nr))
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
    pub thumb_path: String,
}

#[tauri::command]
pub fn get_group_thumbnails(
    group_keys: Vec<String>,
    level: u8,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<GroupThumbDto>, String> {
    let cache_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("thumbnails");
    let mut out = Vec::new();
    for gk in group_keys {
        let imgs = state.db.list_group_images(&gk, level).map_err(|e| e.to_string())?;
        let paths: Vec<PathBuf> = imgs.iter().map(|i| PathBuf::from(&i.abs_path)).collect();
        if paths.is_empty() {
            continue;
        }
        let thumb = crate::thumbnails::get_or_create_group_thumbnail(&gk, &paths, &cache_dir)
            .map_err(|e| e.to_string())?;
        out.push(GroupThumbDto {
            group_key: gk,
            thumb_path: thumb.to_string_lossy().to_string(),
        });
    }
    Ok(out)
}
