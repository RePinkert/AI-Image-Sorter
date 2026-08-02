use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct Db(pub Mutex<Connection>);

static EVENT_COUNTER: AtomicU64 = AtomicU64::new(1);
const MAX_TELEMETRY_PAYLOAD_BYTES: usize = 16 * 1024;

fn next_id(prefix: &str) -> String {
    format!(
        "{}-{}-{:x}",
        prefix,
        Utc::now().timestamp_micros(),
        EVENT_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn validate_token(name: &str, value: &str, max_len: usize, allow_empty: bool) -> Result<()> {
    if value.is_empty() && allow_empty {
        return Ok(());
    }
    if value.is_empty() || value.len() > max_len {
        return Err(anyhow::anyhow!("{name} has invalid length"));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
    {
        return Err(anyhow::anyhow!("{name} contains unsupported characters"));
    }
    Ok(())
}

fn normalize_started_at(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    if value.len() > 64 {
        return Err(anyhow::anyhow!("started_at is too long"));
    }
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|_| anyhow::anyhow!("started_at must be RFC3339"))?;
    Ok(Some(parsed.to_rfc3339()))
}

fn action_duration_ms(started_at: Option<&str>, committed_at: &str) -> Option<i64> {
    let started = started_at.and_then(|s| DateTime::parse_from_rfc3339(s).ok());
    let committed = DateTime::parse_from_rfc3339(committed_at).ok()?;
    Some((committed.signed_duration_since(started?).num_milliseconds()).max(0))
}

fn contains_sensitive_json_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            key.contains("prompt") || key.contains("path") || contains_sensitive_json_key(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_sensitive_json_key),
        _ => false,
    }
}

fn insert_score_tx(tx: &Transaction<'_>, image_id: i64, score: f64, mode: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO scores (image_id, internal_score, updated_at, last_mode)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(image_id) DO UPDATE SET internal_score=excluded.internal_score,
             updated_at=excluded.updated_at, last_mode=excluded.last_mode",
        rusqlite::params![image_id, score, Utc::now().to_rfc3339(), mode],
    )?;
    Ok(())
}

fn current_score_tx(tx: &Transaction<'_>, image_id: i64) -> Result<Option<f64>> {
    Ok(tx
        .query_row(
            "SELECT internal_score FROM scores WHERE image_id=?1",
            rusqlite::params![image_id],
            |r| r.get(0),
        )
        .optional()?)
}

fn insert_review_action(
    tx: &Transaction<'_>,
    action_id: &str,
    session_id: Option<&str>,
    mode: &str,
    image_id: Option<i64>,
    left_image_id: Option<i64>,
    right_image_id: Option<i64>,
    gesture: Option<&str>,
    winner: Option<i64>,
    group_key: Option<&str>,
    started_at: Option<&str>,
    committed_at: &str,
    context_signature: Option<&str>,
    result_json: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO review_actions (
             action_id, session_id, mode, image_id, left_image_id, right_image_id,
             gesture, winner, group_key, started_at, committed_at, duration_ms,
             context_signature, result_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        rusqlite::params![
            action_id,
            session_id,
            mode,
            image_id,
            left_image_id,
            right_image_id,
            gesture,
            winner,
            group_key,
            started_at,
            committed_at,
            action_duration_ms(started_at, committed_at),
            context_signature,
            result_json,
        ],
    )?;
    Ok(())
}

pub fn open(path: &std::path::Path) -> Result<Db> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    migrations::run(&conn)?;
    Ok(Db(Mutex::new(conn)))
}

impl Db {
    /// Run a WAL TRUNCATE checkpoint. Called on shutdown in lib.rs so the
    /// main `.db` file holds all committed data even after an ungraceful
    /// next-launch, preventing the "records lost on restart" symptom.
    pub fn checkpoint(&self) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// Return (image_id, abs_path) for all images currently stored under a
    /// source. Used by `add_source_and_scan` to do idempotent rescans: we
    /// keep existing rows verbatim (preserving scores/labels via their
    /// unchanged FK), only insert brand-new files, and delete rows whose
    /// abs_path is no longer on disk.
    pub fn list_source_image_paths(&self, source_id: i64) -> Result<Vec<(i64, String)>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, abs_path FROM images WHERE source_id=?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(rusqlite::params![source_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Delete images under a source whose abs_path is NOT in `keep`. Scores
    /// and labels cascade via ON DELETE CASCADE. Used only for genuine
    /// filesystem removals during a rescan. `keep` holds normalized paths
    /// (see `commands::normalize_path`); we normalize stored abs_path the
    /// same way before comparing so rescan matching is shape-invariant.
    /// Returns `(removed, affected L1 keys)` so callers know which workflow
    /// groups need re-clustering.
    pub fn delete_missing_source_images(
        &self,
        source_id: i64,
        keep: &std::collections::HashSet<String>,
    ) -> Result<(usize, Vec<String>)> {
        let conn = self.0.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, abs_path, group_key_l1 FROM images WHERE source_id=?1")?;
        let rows = stmt.query_map(rusqlite::params![source_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        })?;
        let mut to_delete: Vec<(i64, Option<String>)> = Vec::new();
        for r in rows {
            let (id, p, l1) = r?;
            let norm = crate::commands::normalize_path_str(&p);
            if !keep.contains(&norm) {
                to_delete.push((id, l1));
            }
        }
        drop(stmt);
        let mut removed = 0usize;
        let mut affected = Vec::new();
        for (id, l1) in &to_delete {
            removed += conn.execute(
                "DELETE FROM images WHERE id=?1",
                rusqlite::params![id],
            )?;
            if let Some(k) = l1 {
                if !k.is_empty() {
                    affected.push(k.clone());
                }
            }
        }
        Ok((removed, affected))
    }
}

mod migrations {
    use super::*;

    pub(crate) const CURRENT_VERSION: i32 = 12;

    pub fn run(conn: &Connection) -> Result<()> {
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // Never drop — all existing data (scores/labels/scan cache) must
        // survive across launches and migrations. `CREATE TABLE IF NOT
        // EXISTS` keeps the schema current; future column additions go via
        // guarded `ALTER TABLE ... ADD COLUMN`.
        create_schema(conn)?;
        seed_default_labels(conn)?;
        let _ = version;
        // One-shot path normalization for rows inserted before this fix
        // (v3 and earlier stored raw abs_path which could differ by case
        // / separator across rescans and silently re-allocate image_id,
        // CASCADE-deleting scores). v4 forces every stored abs_path through
        // commands::normalize_path_str so future rescans match identically.
        if version < 6 {
            normalize_stored_paths(conn)?;
        }
        if version < 7 {
            rebuild_l0_group_keys(conn)?;
        }
        // v5: add images.hidden column (sqlite ALTER TABLE supports adding
        // a column with a DEFAULT + NOT NULL as long as the default is a
        // constant, which 0 is). CREATE TABLE IF NOT EXISTS above already
        // has the column for fresh DBs.
        if version < 5 {
            let has_hidden: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('images') WHERE name='hidden'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if has_hidden == 0 {
                conn.execute_batch(
                    "ALTER TABLE images ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
        }
        // v8/v9: diffusion model / model chain / workflow identity columns
        // (guarded — the dev web bridge may have already added some).
        let cols = image_columns(conn)?;
        if !cols.contains("diffusion_model") {
            conn.execute_batch("ALTER TABLE images ADD COLUMN diffusion_model TEXT;")?;
        }
        if !cols.contains("model_chain_json") {
            conn.execute_batch(
                "ALTER TABLE images ADD COLUMN model_chain_json TEXT NOT NULL DEFAULT '[]';",
            )?;
        }
        if !cols.contains("workflow_key") {
            conn.execute_batch("ALTER TABLE images ADD COLUMN workflow_key TEXT;")?;
        }
        if !cols.contains("workflow_graph_json") {
            conn.execute_batch("ALTER TABLE images ADD COLUMN workflow_graph_json TEXT;")?;
        }
        if !cols.contains("workflow_template_id") {
            conn.execute_batch("ALTER TABLE images ADD COLUMN workflow_template_id INTEGER;")?;
        }
        if !cols.contains("workflow_match_confidence") {
            conn.execute_batch("ALTER TABLE images ADD COLUMN workflow_match_confidence REAL;")?;
        }
        if !cols.contains("generation_recipe_json") {
            conn.execute_batch(
                "ALTER TABLE images ADD COLUMN generation_recipe_json TEXT NOT NULL DEFAULT '{}';",
            )?;
        }
        if !cols.contains("recipe_signature") {
            conn.execute_batch("ALTER TABLE images ADD COLUMN recipe_signature TEXT;")?;
        }
        if !cols.contains("parser_version") {
            conn.execute_batch(
                "ALTER TABLE images ADD COLUMN parser_version TEXT NOT NULL DEFAULT '';",
            )?;
        }
        let src_cols = source_columns(conn)?;
        if !src_cols.contains("file_count") {
            conn.execute_batch(
                "ALTER TABLE sources ADD COLUMN file_count INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if !src_cols.contains("newest_mtime") {
            conn.execute_batch(
                "ALTER TABLE sources ADD COLUMN newest_mtime INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        // v11: the user's L2 Jaccard threshold is persisted per-source so the
        // background sync re-clusters at the SAME threshold the user applied
        // (previously it hardcoded DEFAULT_L2_THRESHOLD, silently undoing
        // every lower-threshold merge on the next image generation).
        if !src_cols.contains("l2_threshold") {
            conn.execute_batch(
                "ALTER TABLE sources ADD COLUMN l2_threshold REAL NOT NULL DEFAULT 0.3;",
            )?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_templates (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                workflow_id TEXT,
                topology_signature TEXT NOT NULL,
                graph_json TEXT NOT NULL,
                node_count INTEGER NOT NULL,
                diffusion_models TEXT NOT NULL DEFAULT '[]',
                model_chain TEXT NOT NULL DEFAULT '[]',
                scanned_at TEXT
            );",
        )?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_images_workflow_key ON images(workflow_key);",
        )?;
        backfill_generation_recipes(conn)?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_images_recipe_signature ON images(recipe_signature);",
        )?;
        conn.execute_batch(&format!("PRAGMA user_version = {};", CURRENT_VERSION))?;
        Ok(())
    }

    fn image_columns(conn: &Connection) -> Result<std::collections::HashSet<String>> {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('images')")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r?);
        }
        Ok(out)
    }

    fn source_columns(conn: &Connection) -> Result<std::collections::HashSet<String>> {
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('sources')")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for r in rows {
            out.insert(r?);
        }
        Ok(out)
    }

    fn create_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sources (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                alias TEXT,
                scanned_at TEXT,
                file_count INTEGER NOT NULL DEFAULT 0,
                newest_mtime INTEGER NOT NULL DEFAULT 0,
                l2_threshold REAL NOT NULL DEFAULT 0.3
            );
            CREATE TABLE IF NOT EXISTS images (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_id INTEGER NOT NULL,
                rel_path TEXT NOT NULL,
                abs_path TEXT NOT NULL,
                filename TEXT NOT NULL,
                size INTEGER,
                width INTEGER,
                height INTEGER,
                prompt_pos TEXT,
                prompt_neg TEXT,
                checkpoint TEXT,
                loras TEXT,
                vae TEXT,
                samplers TEXT,
                seed INTEGER,
                steps INTEGER,
                cfg REAL,
                group_key_l0 TEXT,
                group_key_l1 TEXT,
                group_key_l2 TEXT,
                group_key_l3 TEXT,
                sha256 TEXT,
                meta_ok INTEGER,
                source_kind TEXT,
                scanned_at TEXT,
                -- Hidden = excluded from swipe/arena scoring, still listed in
                -- FolderView with a gray overlay so the user can un-block it.
                hidden INTEGER NOT NULL DEFAULT 0,
                diffusion_model TEXT,
                model_chain_json TEXT NOT NULL DEFAULT '[]',
                 workflow_key TEXT,
                 workflow_graph_json TEXT,
                 workflow_template_id INTEGER,
                 workflow_match_confidence REAL,
                 generation_recipe_json TEXT NOT NULL DEFAULT '{}',
                 recipe_signature TEXT,
                 parser_version TEXT NOT NULL DEFAULT '',
                 FOREIGN KEY(source_id) REFERENCES sources(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_images_l0 ON images(group_key_l0);
            CREATE INDEX IF NOT EXISTS idx_images_l1 ON images(group_key_l1);
            CREATE INDEX IF NOT EXISTS idx_images_l2 ON images(group_key_l2);
            CREATE INDEX IF NOT EXISTS idx_images_l3 ON images(group_key_l3);
            CREATE INDEX IF NOT EXISTS idx_images_source ON images(source_id);
            CREATE TABLE IF NOT EXISTS labels (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                gesture TEXT NOT NULL,
                color TEXT
            );
            CREATE TABLE IF NOT EXISTS image_labels (
                image_id INTEGER NOT NULL,
                label_id INTEGER NOT NULL,
                PRIMARY KEY(image_id, label_id),
                FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE,
                FOREIGN KEY(label_id) REFERENCES labels(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS scores (
                image_id INTEGER PRIMARY KEY,
                internal_score REAL NOT NULL,
                updated_at TEXT,
                last_mode TEXT
            );
            CREATE TABLE IF NOT EXISTS compare_pairs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                group_key TEXT,
                left_img INTEGER,
                right_img INTEGER,
                winner INTEGER,
                delta REAL,
                created_at TEXT
            );
            CREATE TABLE IF NOT EXISTS progress (
                source_id INTEGER,
                group_key TEXT,
                cursor INTEGER,
                PRIMARY KEY(source_id, group_key)
            );
            CREATE TABLE IF NOT EXISTS archive_log (
                image_id INTEGER PRIMARY KEY,
                dest_path TEXT,
                archived_at TEXT
            );
            -- v10: manual L2 group overrides. A row pins an image to a fixed
            -- prompt-deviation group key so `recluster_l2` never re-splits
            -- manually merged groups or re-absorbs manually split images.
            -- kind: 'merge' (every member of merged groups pinned) | 'split'
             CREATE TABLE IF NOT EXISTS manual_group_bindings (
                level INTEGER NOT NULL,
                image_id INTEGER NOT NULL,
                group_key TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'merge',
                created_at TEXT,
                 PRIMARY KEY(level, image_id),
                 FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS review_actions (
                 action_id TEXT PRIMARY KEY NOT NULL,
                 session_id TEXT,
                 mode TEXT NOT NULL,
                 image_id INTEGER,
                 left_image_id INTEGER,
                 right_image_id INTEGER,
                 gesture TEXT,
                 winner INTEGER,
                 group_key TEXT,
                 started_at TEXT,
                 committed_at TEXT NOT NULL,
                 duration_ms INTEGER,
                 context_signature TEXT,
                 result_json TEXT NOT NULL DEFAULT '{}'
             );
             CREATE INDEX IF NOT EXISTS idx_review_actions_session_time
                 ON review_actions(session_id, committed_at);
             CREATE INDEX IF NOT EXISTS idx_review_actions_mode_time
                 ON review_actions(mode, committed_at);
             CREATE INDEX IF NOT EXISTS idx_review_actions_image_time
                 ON review_actions(image_id, committed_at);
             CREATE INDEX IF NOT EXISTS idx_review_actions_pair_time
                 ON review_actions(left_image_id, right_image_id, committed_at);
             CREATE INDEX IF NOT EXISTS idx_review_actions_right_time
                 ON review_actions(right_image_id, committed_at);
             CREATE INDEX IF NOT EXISTS idx_review_actions_group_time
                 ON review_actions(group_key, committed_at);
             CREATE TRIGGER IF NOT EXISTS review_actions_no_update
                 BEFORE UPDATE ON review_actions BEGIN
                 SELECT RAISE(ABORT, 'review_actions is append-only');
             END;
             CREATE TRIGGER IF NOT EXISTS review_actions_no_delete
                 BEFORE DELETE ON review_actions BEGIN
                 SELECT RAISE(ABORT, 'review_actions is append-only');
             END;
             CREATE TABLE IF NOT EXISTS telemetry_events (
                 event_id TEXT PRIMARY KEY NOT NULL,
                 session_id TEXT,
                 event_name TEXT NOT NULL,
                 schema_version TEXT NOT NULL,
                 occurred_at TEXT NOT NULL,
                 mode TEXT,
                 payload_json TEXT NOT NULL DEFAULT '{}',
                 severity TEXT NOT NULL DEFAULT 'info'
             );
             CREATE INDEX IF NOT EXISTS idx_telemetry_events_session_time
                 ON telemetry_events(session_id, occurred_at);
             CREATE INDEX IF NOT EXISTS idx_telemetry_events_name_time
                 ON telemetry_events(event_name, occurred_at);
             CREATE INDEX IF NOT EXISTS idx_telemetry_events_mode_time
                 ON telemetry_events(mode, occurred_at);
             CREATE TRIGGER IF NOT EXISTS telemetry_events_no_update
                 BEFORE UPDATE ON telemetry_events BEGIN
                 SELECT RAISE(ABORT, 'telemetry_events is append-only');
             END;
             CREATE TRIGGER IF NOT EXISTS telemetry_events_no_delete
                 BEFORE DELETE ON telemetry_events BEGIN
                 SELECT RAISE(ABORT, 'telemetry_events is append-only');
             END;
             ",
        )?;
        Ok(())
    }

    fn backfill_generation_recipes(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT id, checkpoint, diffusion_model, loras, vae, samplers,
                    width, height, generation_recipe_json
             FROM images
             WHERE generation_recipe_json IS NULL OR generation_recipe_json='' OR generation_recipe_json='{}'",
        )?;
        let rows: Vec<(
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        )> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);

        for (id, checkpoint, diffusion_model, loras_json, vae, samplers_json, width, height, _) in
            rows
        {
            let loras: Vec<crate::metadata::LoraInfo> = loras_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let sampler = samplers_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<crate::metadata::SamplerInfo>>(s).ok())
                .and_then(|v| v.into_iter().next())
                .unwrap_or_default();
            let recipe = crate::metadata::GenerationRecipe {
                checkpoint: checkpoint.unwrap_or_default(),
                diffusion_model: diffusion_model.unwrap_or_default(),
                loras,
                vae: vae.unwrap_or_default(),
                sampler: sampler.sampler,
                scheduler: sampler.scheduler,
                steps: sampler.steps,
                cfg: sampler.cfg,
                width: width.unwrap_or(0).max(0) as u32,
                height: height.unwrap_or(0).max(0) as u32,
                aspect_ratio: 0.0,
            }
            .normalized();
            let json = serde_json::to_string(&recipe).unwrap_or_else(|_| "{}".to_string());
            let signature = recipe.signature();
            conn.execute(
                "UPDATE images SET generation_recipe_json=?1, recipe_signature=?2,
                    parser_version=?3 WHERE id=?4",
                rusqlite::params![json, signature, "legacy-backfill-v1", id],
            )?;
        }
        Ok(())
    }

    fn seed_default_labels(conn: &Connection) -> Result<()> {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM labels", [], |r| r.get(0))?;
        if count == 0 {
            let defaults = [
                ("差", "left", "#e57373"),
                ("优", "right", "#81c784"),
                ("待优化", "up", "#ffb74d"),
                ("跳过", "down", "#bdbdbd"),
            ];
            for (name, gesture, color) in defaults {
                conn.execute(
                    "INSERT INTO labels (name, gesture, color) VALUES (?1, ?2, ?3)",
                    rusqlite::params![name, gesture, color],
                )?;
            }
        }
        Ok(())
    }

    /// Rewrite every stored abs_path (and the sources.path row that owns
    /// them) through `commands::normalize_path_str` so future rescans match
    /// rows identically. Idempotent — re-running on already-normal data is
    /// a no-op. Collapse duplicate sources that normalize to the same path
    /// by keeping the lowest id and repointing images; orphaned sources are
    /// deleted (their scores already cascade with images).
    fn normalize_stored_paths(conn: &Connection) -> Result<()> {
        // Images.
        let mut stmt = conn.prepare("SELECT id, abs_path FROM images")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (id, p) in rows {
            let n = crate::commands::normalize_path_str(&p);
            if n != p {
                conn.execute(
                    "UPDATE images SET abs_path=?1 WHERE id=?2",
                    rusqlite::params![n, id],
                )?;
            }
        }
        // Sources path.
        let mut stmt = conn.prepare("SELECT id, path FROM sources")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for (id, p) in rows {
            let n = crate::commands::normalize_path_str(&p);
            if n != p {
                // De-dup: if normalization collides with another source's
                // path, repoint images to the lower id and delete the dup.
                let existing: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM sources WHERE path=?1 AND id<>?2 ORDER BY id LIMIT 1",
                        rusqlite::params![n, id],
                        |r| r.get(0),
                    )
                    .ok();
                if let Some(keep_id) = existing {
                    let keep_id = if keep_id < id { keep_id } else { id };
                    let drop_id = if keep_id == id { keep_id } else { id };
                    if drop_id != keep_id {
                        conn.execute(
                            "UPDATE images SET source_id=?1 WHERE source_id=?2",
                            rusqlite::params![keep_id, drop_id],
                        )?;
                        conn.execute(
                            "DELETE FROM sources WHERE id=?1",
                            rusqlite::params![drop_id],
                        )?;
                    }
                    conn.execute(
                        "UPDATE sources SET path=?1 WHERE id=?2",
                        rusqlite::params![n, keep_id],
                    )?;
                } else {
                    conn.execute(
                        "UPDATE sources SET path=?1 WHERE id=?2",
                        rusqlite::params![n, id],
                    )?;
                }
            }
        }
        Ok(())
    }

    /// L0 is derived from the source folder path. Older rows can retain a
    /// hash of the pre-normalized path even after the stored path is fixed,
    /// which makes one physical folder appear as multiple groups.
    fn rebuild_l0_group_keys(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("SELECT id, path FROM sources")?;
        let sources: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        for (source_id, path) in sources {
            let key = format!(
                "{:016x}",
                xxhash_rust::xxh3::xxh3_64(path.as_bytes())
            );
            conn.execute(
                "UPDATE images SET group_key_l0=?1 WHERE source_id=?2",
                rusqlite::params![key, source_id],
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRow {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub alias: Option<String>,
    pub scanned_at: Option<String>,
    /// Persisted L2 Jaccard threshold this source was last re-clustered at.
    /// The background sync uses this instead of a hardcoded default so it
    /// never undoes the user's threshold choice.
    pub l2_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRow {
    pub id: i64,
    pub source_id: i64,
    pub abs_path: String,
    pub filename: String,
    pub width: i64,
    pub height: i64,
    pub prompt_pos: String,
    pub prompt_neg: String,
    pub checkpoint: String,
    pub loras: String,
    pub vae: String,
    pub samplers: String,
    pub seed: i64,
    pub meta_ok: bool,
    pub source_kind: String,
    pub score: Option<f64>,
    pub labels: Vec<LabelRow>,
    /// Hidden = user-flagged for exclusion from scoring (swipe/arena);
    /// still listed in FolderView with a gray overlay so it can be un-blocked.
    pub hidden: bool,
    /// File size in bytes (for folder-view sorting).
    pub size: i64,
    /// Primary diffusion model (v8+). Empty for legacy rows until reparse.
    pub diffusion_model: String,
    /// Saved-template name this image's workflow matched (if any).
    pub workflow_name: Option<String>,
    /// When the image is pinned to a manual L2 group override, the binding
    /// kind ('merge' | 'split'); None when it follows auto-clustering.
    pub manually_grouped: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFacet {
    pub model: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelRow {
    pub id: i64,
    pub name: String,
    pub gesture: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInfo {
    pub group_key: String,
    pub count: i64,
    pub prompt_pos: String,
    pub checkpoint: String,
    pub source_kind: String,
    pub source_path: String,
    /// Template name for the workflow level (granularity 1), else None.
    pub workflow_name: Option<String>,
    /// Distinct diffusion models + image counts for the workflow level.
    pub model_facets: Vec<ModelFacet>,
    /// True when the group's members are pinned by a manual L2 override.
    pub manually_merged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTemplateRow {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub workflow_id: Option<String>,
    pub topology_signature: String,
    pub graph_json: String,
    pub node_count: i64,
    pub diffusion_models: String,
    pub model_chain: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptRecommendation {
    pub prompt_text: String,
    pub prompt_neg: String,
    pub diffusion_model: String,
    pub checkpoint: String,
    pub loras: Vec<crate::metadata::LoraInfo>,
    pub vae: String,
    pub sampler: String,
    pub scheduler: String,
    pub steps: i64,
    pub cfg: f64,
    pub width: i64,
    pub height: i64,
    pub aspect_ratio: f64,
    pub sample_count: i64,
    pub max_score: f64,
    pub avg_score: f64,
    pub median_score: f64,
    pub score_variance: f64,
    pub confidence: f64,
    pub example_image_ids: Vec<i64>,
    /// Kept as an alias for existing callers while they migrate to
    /// `sample_count`.
    pub image_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub action_id: String,
    pub image_id: i64,
    pub score: f64,
    pub hidden: bool,
    pub label_id: Option<i64>,
    pub committed_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEventResult {
    pub event_id: String,
}

impl Db {
    pub fn upsert_source(&self, path: &str, kind: &str, alias: Option<&str>) -> Result<i64> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO sources (path, kind, alias, scanned_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET kind=excluded.kind, alias=excluded.alias, scanned_at=excluded.scanned_at",
            rusqlite::params![path, kind, alias, Utc::now().to_rfc3339()],
        )?;
        let id: i64 = conn.query_row(
            "SELECT id FROM sources WHERE path=?1",
            rusqlite::params![path],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn list_sources(&self) -> Result<Vec<SourceRow>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, path, kind, alias, scanned_at, l2_threshold FROM sources ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                path: r.get(1)?,
                kind: r.get(2)?,
                alias: r.get(3)?,
                scanned_at: r.get(4)?,
                l2_threshold: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Persist the L2 Jaccard threshold a source was re-clustered at. The
    /// background sync reads this back so auto-re-clustering matches what
    /// the user applied.
    pub fn set_source_threshold(&self, source_id: i64, threshold: f64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE sources SET l2_threshold=?2 WHERE id=?1",
            rusqlite::params![source_id, threshold],
        )?;
        Ok(())
    }

    /// The persisted L2 threshold for a source (defaults to the clustering
    /// default for rows created before v11).
    pub fn get_source_threshold(&self, source_id: i64) -> Result<f64> {
        let conn = self.0.lock().unwrap();
        let t = conn
            .query_row(
                "SELECT l2_threshold FROM sources WHERE id=?1",
                rusqlite::params![source_id],
                |r| r.get::<_, f64>(0),
            )
            .unwrap_or(crate::clustering::DEFAULT_L2_THRESHOLD);
        Ok(t)
    }

    pub fn insert_image(&self, row: &ImageInsert) -> Result<i64> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO images (
                source_id, rel_path, abs_path, filename, size, width, height,
                prompt_pos, prompt_neg, checkpoint, loras, vae, samplers, seed, steps, cfg,
                group_key_l0, group_key_l1, group_key_l2, group_key_l3,
                 sha256, meta_ok, source_kind, scanned_at,
                 diffusion_model, model_chain_json,
                 workflow_key, workflow_graph_json,
                 workflow_template_id, workflow_match_confidence,
                 generation_recipe_json, recipe_signature, parser_version
             ) VALUES (
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,?18,?19,?20,?21,?22,?23,?24,
                 ?25,?26,?27,?28,?29,?30,?31,?32,?33
             )",
            rusqlite::params![
                row.source_id,
                row.rel_path,
                row.abs_path,
                row.filename,
                row.size,
                row.width,
                row.height,
                row.prompt_pos,
                row.prompt_neg,
                row.checkpoint,
                row.loras,
                row.vae,
                row.samplers,
                row.seed,
                row.steps,
                row.cfg,
                row.group_key_l0,
                row.group_key_l1,
                row.group_key_l2,
                row.group_key_l3,
                row.sha256,
                row.meta_ok as i64,
                row.source_kind,
                Utc::now().to_rfc3339(),
                row.diffusion_model,
                row.model_chain_json,
                row.workflow_key,
                row.workflow_graph_json,
                row.workflow_template_id,
                row.workflow_match_confidence,
                row.generation_recipe_json,
                row.recipe_signature,
                row.parser_version,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Full metadata refresh for a row re-parsed from disk (backfill after a
    /// parser fix or a mid-write failed scan). Preserves image id, scores,
    /// labels, and archive log. Returns the new L1 key (for re-clustering).
    pub fn update_reparsed_image(
        &self,
        image_id: i64,
        row: &ImageInsert,
        source_id: i64,
    ) -> Result<String> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE images SET
                source_id=?2, prompt_pos=?3, prompt_neg=?4, checkpoint=?5,
                loras=?6, vae=?7, samplers=?8, seed=?9, steps=?10, cfg=?11,
                group_key_l0=?12, group_key_l1=?13, group_key_l2=?14, group_key_l3=?15,
                 meta_ok=?16, source_kind=?17, width=?18, height=?19,
                 diffusion_model=?20, model_chain_json=?21,
                 workflow_key=?22, workflow_graph_json=?23,
                 workflow_template_id=?24, workflow_match_confidence=?25,
                 generation_recipe_json=?26, recipe_signature=?27, parser_version=?28
             WHERE id=?1",
            rusqlite::params![
                image_id,
                source_id,
                row.prompt_pos,
                row.prompt_neg,
                row.checkpoint,
                row.loras,
                row.vae,
                row.samplers,
                row.seed,
                row.steps,
                row.cfg,
                row.group_key_l0,
                row.group_key_l1,
                row.group_key_l2,
                row.group_key_l3,
                row.meta_ok as i64,
                row.source_kind,
                row.width,
                row.height,
                row.diffusion_model,
                row.model_chain_json,
                row.workflow_key,
                row.workflow_graph_json,
                row.workflow_template_id,
                row.workflow_match_confidence,
                row.generation_recipe_json,
                row.recipe_signature,
                row.parser_version,
            ],
        )?;
        Ok(row.group_key_l1.clone())
    }

    pub fn list_groups(&self, source_id: Option<i64>, level: u8) -> Result<Vec<GroupInfo>> {
        let conn = self.0.lock().unwrap();
        let key_col = match level {
            0 => "group_key_l0",
            1 => "group_key_l1",
            2 => "group_key_l2",
            _ => "group_key_l3",
        };
        // Hidden rows are excluded from group counts — the user has decided
        // these images no longer participate in the scoring workflow, so
        // surfacing them in a group thumbnail would be misleading.
        let sql = match source_id {
            Some(sid) => format!(
                "SELECT {col} AS gk, COUNT(*) c, MIN(i.prompt_pos) pp, MIN(i.checkpoint) ck, MIN(i.source_kind) sk, MIN(s.path) sp, MIN(wt.name) wname
                 FROM images i JOIN sources s ON s.id=i.source_id
                 LEFT JOIN workflow_templates wt ON wt.id=i.workflow_template_id
                 WHERE i.source_id={sid} AND {col} IS NOT NULL AND i.hidden=0
                 GROUP BY {col} ORDER BY c DESC",
                col = key_col, sid = sid
            ),
            None => format!(
                "SELECT {col} AS gk, COUNT(*) c, MIN(i.prompt_pos) pp, MIN(i.checkpoint) ck, MIN(i.source_kind) sk, MIN(s.path) sp, MIN(wt.name) wname
                 FROM images i JOIN sources s ON s.id=i.source_id
                 LEFT JOIN workflow_templates wt ON wt.id=i.workflow_template_id
                 WHERE {col} IS NOT NULL AND i.hidden=0
                 GROUP BY {col} ORDER BY c DESC",
                col = key_col
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row| {
            Ok(GroupInfo {
                group_key: r.get(0)?,
                count: r.get(1)?,
                prompt_pos: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                checkpoint: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                source_kind: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                source_path: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                workflow_name: r.get(6)?,
                model_facets: Vec::new(),
                manually_merged: false,
            })
        };
        let mut out = Vec::new();
        let rows = stmt.query_map([], map)?;
        for r in rows {
            out.push(r?);
        }
        // Model facets only make sense at the workflow level.
        if level == 1 {
            for group in &mut out {
                group.model_facets = self.model_facets_for_group(&conn, &group.group_key)?;
            }
        }
        // Manual-merge badge only makes sense at the prompt-deviation level.
        if level == 2 {
            for group in &mut out {
                group.manually_merged = self.group_has_binding(&conn, 2, &group.group_key)?;
            }
        }
        Ok(out)
    }

    fn model_facets_for_group(
        &self,
        conn: &Connection,
        group_key: &str,
    ) -> Result<Vec<ModelFacet>> {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(NULLIF(diffusion_model, ''), '(未知)') AS model, COUNT(*) c
             FROM images WHERE group_key_l1=?1 AND hidden=0
             GROUP BY model ORDER BY c DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![group_key], |r| {
            Ok(ModelFacet {
                model: r.get(0)?,
                count: r.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Swipe/Arena view: hidden rows are filtered OUT.
    pub fn list_group_images(&self, group_key: &str, level: u8) -> Result<Vec<ImageRow>> {
        self.list_group_images_inner(group_key, level, false)
    }

    /// Return up to four original image paths for a group. The frontend uses
    /// the original files directly as the group preview, so no generated
    /// cache or image decoding is needed for the group list.
    pub fn list_group_thumbnail_paths(&self, group_key: &str, level: u8) -> Result<Vec<String>> {
        let conn = self.0.lock().unwrap();
        let key_col = match level {
            0 => "group_key_l0",
            1 => "group_key_l1",
            2 => "group_key_l2",
            _ => "group_key_l3",
        };
        let sql = format!(
            "SELECT abs_path FROM images WHERE {col}=?1 AND hidden=0
             ORDER BY seed, filename LIMIT 4",
            col = key_col
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![group_key], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// FolderView: includes hidden rows so the user can see / un-block them.
    pub fn list_group_images_all(&self, group_key: &str, level: u8) -> Result<Vec<ImageRow>> {
        self.list_group_images_inner(group_key, level, true)
    }

    fn list_group_images_inner(
        &self,
        group_key: &str,
        level: u8,
        include_hidden: bool,
    ) -> Result<Vec<ImageRow>> {
        let conn = self.0.lock().unwrap();
        let key_col = match level {
            0 => "group_key_l0",
            1 => "group_key_l1",
            2 => "group_key_l2",
            _ => "group_key_l3",
        };
        let hidden_clause = if include_hidden { "" } else { "AND hidden=0" };
        let sql = format!(
            "SELECT i.id, i.source_id, i.abs_path, i.filename, i.width, i.height, i.prompt_pos, i.prompt_neg,
                    i.checkpoint, i.loras, i.vae, i.samplers, i.seed, i.meta_ok, i.source_kind, i.hidden, i.size,
                    COALESCE(NULLIF(i.diffusion_model, ''), ''), wt.name, b.kind
             FROM images i LEFT JOIN workflow_templates wt ON wt.id=i.workflow_template_id
             LEFT JOIN manual_group_bindings b ON b.level=2 AND b.image_id=i.id
             WHERE i.{col}=?1 {hid} ORDER BY i.seed, i.filename",
            col = key_col, hid = hidden_clause
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![group_key], |r| {
            Ok(ImageRow {
                id: r.get(0)?,
                source_id: r.get(1)?,
                abs_path: r.get(2)?,
                filename: r.get(3)?,
                width: r.get(4)?,
                height: r.get(5)?,
                prompt_pos: r.get(6)?,
                prompt_neg: r.get(7)?,
                checkpoint: r.get(8)?,
                loras: r.get(9)?,
                vae: r.get(10)?,
                samplers: r.get(11)?,
                seed: r.get(12)?,
                meta_ok: r.get::<_, i64>(13)? != 0,
                source_kind: r.get(14)?,
                score: None,
                labels: Vec::new(),
                hidden: r.get::<_, i64>(15)? != 0,
                size: r.get::<_, Option<i64>>(16)?.unwrap_or(0),
                diffusion_model: r.get::<_, Option<String>>(17)?.unwrap_or_default(),
                workflow_name: r.get(18)?,
                manually_grouped: r.get(19)?,
            })
        })?;
        let mut out: Vec<ImageRow> = Vec::new();
        for r in rows {
            out.push(r?);
        }
        for img in &mut out {
            let score: Option<f64> = conn
                .query_row(
                    "SELECT internal_score FROM scores WHERE image_id=?1",
                    rusqlite::params![img.id],
                    |r| r.get(0),
                )
                .ok();
            img.score = score;
            img.labels = self.labels_for(&conn, img.id)?;
        }
        Ok(out)
    }

    /// Look up the abs_path for an image (used by trash_image to find the
    /// file to send to the recycle bin). Returns None if the row is gone.
    pub fn image_abs_path(&self, image_id: i64) -> Result<Option<String>> {
        let conn = self.0.lock().unwrap();
        let res: Option<String> = conn
            .query_row(
                "SELECT abs_path FROM images WHERE id=?1",
                rusqlite::params![image_id],
                |r| r.get(0),
            )
            .ok();
        Ok(res)
    }

    /// Delete an image row by id. Scores / labels cascade-delete via the
    /// image_id FK ON DELETE CASCADE. Used after the underlying file has
    /// been moved to the OS recycle bin (trash_image).
    pub fn delete_image_by_id(&self, image_id: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "DELETE FROM images WHERE id=?1",
            rusqlite::params![image_id],
        )?;
        Ok(())
    }

    /// Recommend only existing, complete prompt + generation-recipe pairs.
    /// Scores are ranked by a mean shrunk toward BASE_SCORE so one lucky
    /// sample does not outrank a well-supported configuration.
    pub fn recommend_prompts(
        &self,
        group_key: &str,
        level: u8,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<PromptRecommendation>> {
        if level >= 3 {
            return Ok(Vec::new());
        }
        let page_offset = offset.max(0);
        let page_limit = limit.max(0).min(100);
        if page_limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self.0.lock().unwrap();
        let key_col = match level {
            0 => "group_key_l0",
            1 => "group_key_l1",
            _ => "group_key_l2",
        };
        let sql = format!(
            "SELECT i.id, i.prompt_pos, COALESCE(i.prompt_neg, ''), i.generation_recipe_json,
                    COALESCE(s.internal_score, 50.0)
             FROM images i
             LEFT JOIN scores s ON s.image_id = i.id
             WHERE i.{col}=?1 AND i.hidden=0 AND i.prompt_pos != ''
               AND i.generation_recipe_json IS NOT NULL
               AND i.generation_recipe_json != '' AND i.generation_recipe_json != '{{}}'",
            col = key_col
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![group_key], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })?;
        struct RecommendationAccum {
            prompt_text: String,
            prompt_neg: String,
            recipe: crate::metadata::GenerationRecipe,
            scored_ids: Vec<(i64, f64)>,
        }
        let mut grouped: HashMap<(String, String, String), RecommendationAccum> = HashMap::new();
        for r in rows {
            let (image_id, prompt_text, prompt_neg, recipe_json, score) = r?;
            let recipe =
                match serde_json::from_str::<crate::metadata::GenerationRecipe>(&recipe_json) {
                    Ok(recipe) => recipe.normalized(),
                    Err(_) => continue,
                };
            if !recipe.is_complete() {
                continue;
            }
            let key = (prompt_text.clone(), prompt_neg.clone(), recipe.signature());
            grouped
                .entry(key)
                .or_insert_with(|| RecommendationAccum {
                    prompt_text,
                    prompt_neg,
                    recipe,
                    scored_ids: Vec::new(),
                })
                .scored_ids
                .push((
                    image_id,
                    if score.is_finite() {
                        score
                    } else {
                        crate::scoring::BASE_SCORE
                    },
                ));
        }
        drop(stmt);

        let mut ranked: Vec<(f64, PromptRecommendation)> = Vec::new();
        for accum in grouped.into_values() {
            let mut scores: Vec<f64> = accum.scored_ids.iter().map(|(_, score)| *score).collect();
            scores.sort_by(f64::total_cmp);
            let sample_count = scores.len() as i64;
            if sample_count == 0 {
                continue;
            }
            let sum: f64 = scores.iter().sum();
            let avg_score = sum / sample_count as f64;
            let median_score = if scores.len() % 2 == 1 {
                scores[scores.len() / 2]
            } else {
                (scores[scores.len() / 2 - 1] + scores[scores.len() / 2]) / 2.0
            };
            let score_variance = scores
                .iter()
                .map(|score| (score - avg_score).powi(2))
                .sum::<f64>()
                / sample_count as f64;
            let confidence = sample_count as f64 / (sample_count as f64 + 5.0);
            let shrink_score =
                avg_score * confidence + crate::scoring::BASE_SCORE * (1.0 - confidence);
            let max_score = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mut examples = accum.scored_ids.clone();
            examples.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            examples.truncate(4);
            let recipe = accum.recipe;
            ranked.push((
                shrink_score,
                PromptRecommendation {
                    prompt_text: accum.prompt_text,
                    prompt_neg: accum.prompt_neg,
                    diffusion_model: recipe.diffusion_model.clone(),
                    checkpoint: recipe.checkpoint.clone(),
                    loras: recipe.loras.clone(),
                    vae: recipe.vae.clone(),
                    sampler: recipe.sampler.clone(),
                    scheduler: recipe.scheduler.clone(),
                    steps: recipe.steps,
                    cfg: recipe.cfg,
                    width: recipe.width as i64,
                    height: recipe.height as i64,
                    aspect_ratio: recipe.aspect_ratio,
                    sample_count,
                    max_score,
                    avg_score,
                    median_score,
                    score_variance,
                    confidence,
                    example_image_ids: examples.into_iter().map(|(id, _)| id).collect(),
                    image_count: sample_count,
                },
            ));
        }
        ranked.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| b.1.avg_score.total_cmp(&a.1.avg_score))
                .then_with(|| b.1.sample_count.cmp(&a.1.sample_count))
        });
        Ok(ranked
            .into_iter()
            .skip(page_offset as usize)
            .take(page_limit as usize)
            .map(|(_, recommendation)| recommendation)
            .collect())
    }

    fn labels_for(&self, conn: &Connection, image_id: i64) -> Result<Vec<LabelRow>> {
        let mut stmt = conn.prepare(
            "SELECT l.id, l.name, l.gesture, l.color FROM labels l
             JOIN image_labels il ON il.label_id=l.id WHERE il.image_id=?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![image_id], |r| {
            Ok(LabelRow {
                id: r.get(0)?,
                name: r.get(1)?,
                gesture: r.get(2)?,
                color: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn list_labels(&self) -> Result<Vec<LabelRow>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, name, gesture, color FROM labels ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(LabelRow {
                id: r.get(0)?,
                name: r.get(1)?,
                gesture: r.get(2)?,
                color: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn upsert_label(&self, id: Option<i64>, name: &str, gesture: &str, color: Option<&str>) -> Result<i64> {
        let conn = self.0.lock().unwrap();
        if let Some(id) = id {
            conn.execute(
                "UPDATE labels SET name=?1, gesture=?2, color=?3 WHERE id=?4",
                rusqlite::params![name, gesture, color, id],
            )?;
            Ok(id)
        } else {
            conn.execute(
                "INSERT INTO labels (name, gesture, color) VALUES (?1, ?2, ?3)",
                rusqlite::params![name, gesture, color],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }

    pub fn delete_label(&self, id: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute("DELETE FROM labels WHERE id=?1", rusqlite::params![id])?;
        Ok(())
    }

    pub fn set_image_label(&self, image_id: i64, label_id: i64, on: bool) -> Result<()> {
        let conn = self.0.lock().unwrap();
        if on {
            conn.execute(
                "INSERT OR IGNORE INTO image_labels (image_id, label_id) VALUES (?1, ?2)",
                rusqlite::params![image_id, label_id],
            )?;
        } else {
            conn.execute(
                "DELETE FROM image_labels WHERE image_id=?1 AND label_id=?2",
                rusqlite::params![image_id, label_id],
            )?;
        }
        Ok(())
    }

    pub fn apply_swipe_action(
        &self,
        image_id: i64,
        gesture: &str,
        label_id: Option<i64>,
        session_id: Option<&str>,
        started_at: Option<&str>,
        context_signature: Option<&str>,
    ) -> Result<ActionResult> {
        if !matches!(gesture, "left" | "right" | "up" | "down") {
            return Err(anyhow::anyhow!("unsupported gesture"));
        }
        if let Some(session_id) = session_id {
            validate_token("session_id", session_id, 128, false)?;
        }
        if let Some(signature) = context_signature {
            validate_token("context_signature", signature, 256, false)?;
        }
        let started_at = normalize_started_at(started_at)?;
        let action_id = next_id("act");
        let committed_at = Utc::now().to_rfc3339();
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        let (group_key, image_hidden): (Option<String>, bool) = tx
            .query_row(
                "SELECT group_key_l3, hidden FROM images WHERE id=?1",
                rusqlite::params![image_id],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("image row not found"))?;
        if let Some(label_id) = label_id {
            let exists: i64 = tx.query_row(
                "SELECT COUNT(*) FROM labels WHERE id=?1",
                rusqlite::params![label_id],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(anyhow::anyhow!("label row not found"));
            }
            tx.execute(
                "INSERT OR IGNORE INTO image_labels (image_id, label_id) VALUES (?1, ?2)",
                rusqlite::params![image_id, label_id],
            )?;
        }
        let current = current_score_tx(&tx, image_id)?;
        let score = crate::scoring::apply_swipe(current, gesture);
        insert_score_tx(&tx, image_id, score, "swipe")?;
        let result_json = serde_json::json!({
            "score": score,
            "label_id": label_id,
            "hidden": image_hidden,
        })
        .to_string();
        insert_review_action(
            &tx,
            &action_id,
            session_id,
            "swipe",
            Some(image_id),
            None,
            None,
            Some(gesture),
            None,
            group_key.as_deref(),
            started_at.as_deref(),
            &committed_at,
            context_signature,
            &result_json,
        )?;
        tx.commit()?;
        Ok(ActionResult {
            action_id,
            image_id,
            score,
            hidden: image_hidden,
            label_id,
            committed_at,
        })
    }

    pub fn arena_vote_atomic(
        &self,
        group_key: &str,
        left: i64,
        right: i64,
        winner_is_left: bool,
        session_id: Option<&str>,
        started_at: Option<&str>,
        context_signature: Option<&str>,
    ) -> Result<(f64, f64)> {
        validate_token("group_key", group_key, 256, true)?;
        if let Some(session_id) = session_id {
            validate_token("session_id", session_id, 128, false)?;
        }
        if let Some(signature) = context_signature {
            validate_token("context_signature", signature, 256, false)?;
        }
        let started_at = normalize_started_at(started_at)?;
        let action_id = next_id("act");
        let committed_at = Utc::now().to_rfc3339();
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        for image_id in [left, right] {
            let exists: i64 = tx.query_row(
                "SELECT COUNT(*) FROM images WHERE id=?1",
                rusqlite::params![image_id],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(anyhow::anyhow!("image row not found"));
            }
        }
        let left_score = current_score_tx(&tx, left)?.unwrap_or(crate::scoring::BASE_SCORE);
        let right_score = current_score_tx(&tx, right)?.unwrap_or(crate::scoring::BASE_SCORE);
        let (new_left, new_right) =
            crate::scoring::apply_arena(left_score, right_score, winner_is_left);
        insert_score_tx(&tx, left, new_left, "arena")?;
        insert_score_tx(&tx, right, new_right, "arena")?;
        let winner = if winner_is_left { left } else { right };
        tx.execute(
            "INSERT INTO compare_pairs (group_key, left_img, right_img, winner, delta, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                group_key,
                left,
                right,
                winner,
                (new_left - left_score).abs(),
                &committed_at,
            ],
        )?;
        let result_json = serde_json::json!({
            "left_score": new_left,
            "right_score": new_right,
            "winner": winner,
        })
        .to_string();
        insert_review_action(
            &tx,
            &action_id,
            session_id,
            "arena",
            None,
            Some(left),
            Some(right),
            None,
            Some(winner),
            Some(group_key),
            started_at.as_deref(),
            &committed_at,
            context_signature,
            &result_json,
        )?;
        tx.commit()?;
        Ok((new_left, new_right))
    }

    pub fn toggle_hidden_atomic(
        &self,
        image_id: i64,
        hidden: bool,
        session_id: Option<&str>,
        started_at: Option<&str>,
        context_signature: Option<&str>,
    ) -> Result<ActionResult> {
        if let Some(session_id) = session_id {
            validate_token("session_id", session_id, 128, false)?;
        }
        if let Some(signature) = context_signature {
            validate_token("context_signature", signature, 256, false)?;
        }
        let started_at = normalize_started_at(started_at)?;
        let action_id = next_id("act");
        let committed_at = Utc::now().to_rfc3339();
        let mut conn = self.0.lock().unwrap();
        let tx = conn.transaction()?;
        let group_key: Option<String> = tx
            .query_row(
                "SELECT group_key_l3 FROM images WHERE id=?1",
                rusqlite::params![image_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("image row not found"))?;
        tx.execute(
            "UPDATE images SET hidden=?1 WHERE id=?2",
            rusqlite::params![hidden as i64, image_id],
        )?;
        let score = if hidden {
            0.0
        } else {
            current_score_tx(&tx, image_id)?.unwrap_or(crate::scoring::BASE_SCORE)
        };
        insert_score_tx(&tx, image_id, score, if hidden { "hide" } else { "unhide" })?;
        let result_json = serde_json::json!({
            "hidden": hidden,
            "score": score,
        })
        .to_string();
        insert_review_action(
            &tx,
            &action_id,
            session_id,
            if hidden { "hide" } else { "unhide" },
            Some(image_id),
            None,
            None,
            None,
            None,
            group_key.as_deref(),
            started_at.as_deref(),
            &committed_at,
            context_signature,
            &result_json,
        )?;
        tx.commit()?;
        Ok(ActionResult {
            action_id,
            image_id,
            score,
            hidden,
            label_id: None,
            committed_at,
        })
    }

    pub fn record_telemetry_event(
        &self,
        session_id: Option<&str>,
        event_name: &str,
        schema_version: &str,
        occurred_at: Option<&str>,
        mode: Option<&str>,
        payload_json: &str,
        severity: Option<&str>,
    ) -> Result<TelemetryEventResult> {
        if event_name.is_empty()
            || event_name.len() > 96
            || !event_name.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
            })
        {
            return Err(anyhow::anyhow!("event_name is not controlled"));
        }
        validate_token("schema_version", schema_version, 32, false)?;
        if let Some(session_id) = session_id {
            validate_token("session_id", session_id, 128, false)?;
        }
        if let Some(mode) = mode {
            validate_token("mode", mode, 32, false)?;
        }
        let severity = severity.unwrap_or("info");
        if !matches!(severity, "debug" | "info" | "warn" | "error") {
            return Err(anyhow::anyhow!("severity is not controlled"));
        }
        if payload_json.as_bytes().len() > MAX_TELEMETRY_PAYLOAD_BYTES {
            return Err(anyhow::anyhow!("telemetry payload is too large"));
        }
        let payload: serde_json::Value = serde_json::from_str(payload_json)
            .map_err(|_| anyhow::anyhow!("payload_json must be valid JSON"))?;
        if contains_sensitive_json_key(&payload) {
            return Err(anyhow::anyhow!(
                "telemetry payload contains a restricted field"
            ));
        }
        let payload_json = serde_json::to_string(&payload)?;
        let occurred_at =
            normalize_started_at(occurred_at)?.unwrap_or_else(|| Utc::now().to_rfc3339());
        let event_id = next_id("evt");
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO telemetry_events
                 (event_id, session_id, event_name, schema_version, occurred_at, mode, payload_json, severity)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                event_id,
                session_id,
                event_name,
                schema_version,
                occurred_at,
                mode,
                payload_json,
                severity,
            ],
        )?;
        Ok(TelemetryEventResult { event_id })
    }

    pub fn export_diagnostics(&self, limit: i64) -> Result<serde_json::Value> {
        let limit = limit.clamp(1, 1000);
        let conn = self.0.lock().unwrap();
        let mut telemetry = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT event_id, session_id, event_name, schema_version, occurred_at,
                    mode, payload_json, severity
             FROM telemetry_events ORDER BY occurred_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], |r| {
            let payload_json: String = r.get(6)?;
            let payload = serde_json::from_str::<serde_json::Value>(&payload_json)
                .unwrap_or_else(|_| serde_json::json!({}));
            Ok(serde_json::json!({
                "event_id": r.get::<_, String>(0)?,
                "session_id": r.get::<_, Option<String>>(1)?,
                "event_name": r.get::<_, String>(2)?,
                "schema_version": r.get::<_, String>(3)?,
                "occurred_at": r.get::<_, String>(4)?,
                "mode": r.get::<_, Option<String>>(5)?,
                "payload": payload,
                "severity": r.get::<_, String>(7)?,
            }))
        })?;
        for row in rows {
            telemetry.push(row?);
        }
        drop(stmt);

        let mut reviews = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT action_id, session_id, mode, image_id, left_image_id,
                    right_image_id, gesture, winner, group_key, started_at,
                    committed_at, duration_ms, context_signature, result_json
             FROM review_actions ORDER BY committed_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], |r| {
            let result_json: String = r.get(13)?;
            let result = serde_json::from_str::<serde_json::Value>(&result_json)
                .unwrap_or_else(|_| serde_json::json!({}));
            Ok(serde_json::json!({
                "action_id": r.get::<_, String>(0)?,
                "session_id": r.get::<_, Option<String>>(1)?,
                "mode": r.get::<_, String>(2)?,
                "image_id": r.get::<_, Option<i64>>(3)?,
                "left_image_id": r.get::<_, Option<i64>>(4)?,
                "right_image_id": r.get::<_, Option<i64>>(5)?,
                "gesture": r.get::<_, Option<String>>(6)?,
                "winner": r.get::<_, Option<i64>>(7)?,
                "group_key": r.get::<_, Option<String>>(8)?,
                "started_at": r.get::<_, Option<String>>(9)?,
                "committed_at": r.get::<_, String>(10)?,
                "duration_ms": r.get::<_, Option<i64>>(11)?,
                "context_signature": r.get::<_, Option<String>>(12)?,
                "result": result,
            }))
        })?;
        for row in rows {
            reviews.push(row?);
        }

        Ok(serde_json::json!({
            "schema_version": "diagnostics-v1",
            "telemetry_events": telemetry,
            "review_actions": reviews,
        }))
    }

    pub fn record_archive(&self, image_id: i64, dest: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO archive_log (image_id, dest_path, archived_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![image_id, dest, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Change-detection counters: `(file_count, newest_mtime_secs)` as of
    /// the last completed scan of this source.
    pub fn get_source_stats(&self, source_id: i64) -> Result<(i64, i64)> {
        let conn = self.0.lock().unwrap();
        let (count, newest) = conn
            .query_row(
                "SELECT file_count, newest_mtime FROM sources WHERE id=?1",
                rusqlite::params![source_id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap_or((0, 0));
        Ok((count, newest))
    }

    pub fn set_source_stats(&self, source_id: i64, file_count: i64, newest_mtime: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "UPDATE sources SET file_count=?2, newest_mtime=?3 WHERE id=?1",
            rusqlite::params![source_id, file_count, newest_mtime],
        )?;
        Ok(())
    }

    pub fn list_workflow_templates(&self) -> Result<Vec<WorkflowTemplateRow>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, path, workflow_id, topology_signature, graph_json, node_count, diffusion_models, model_chain
             FROM workflow_templates ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WorkflowTemplateRow {
                id: r.get(0)?,
                name: r.get(1)?,
                path: r.get(2)?,
                workflow_id: r.get(3)?,
                topology_signature: r.get(4)?,
                graph_json: r.get(5)?,
                node_count: r.get(6)?,
                diffusion_models: r.get(7)?,
                model_chain: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn upsert_workflow_template(
        &self,
        name: &str,
        path: &str,
        workflow_id: Option<&str>,
        signature: &str,
        graph_json: &str,
        node_count: usize,
        diffusion_models: &[String],
        model_chain: &[String],
    ) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO workflow_templates
                (name, path, workflow_id, topology_signature, graph_json, node_count, diffusion_models, model_chain, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(path) DO UPDATE SET
                name=excluded.name, workflow_id=excluded.workflow_id,
                topology_signature=excluded.topology_signature, graph_json=excluded.graph_json,
                node_count=excluded.node_count, diffusion_models=excluded.diffusion_models,
                model_chain=excluded.model_chain, scanned_at=excluded.scanned_at",
            rusqlite::params![
                name,
                path,
                workflow_id,
                signature,
                graph_json,
                node_count as i64,
                serde_json::to_string(diffusion_models).unwrap_or_else(|_| "[]".to_string()),
                serde_json::to_string(model_chain).unwrap_or_else(|_| "[]".to_string()),
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Delete template rows whose file vanished from disk.
    pub fn prune_workflow_templates(&self, keep_paths: &std::collections::HashSet<String>) -> Result<usize> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, path FROM workflow_templates")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut to_delete = Vec::new();
        for r in rows {
            let (id, p) = r?;
            if !keep_paths.contains(&p) {
                to_delete.push(id);
            }
        }
        drop(stmt);
        let mut removed = 0usize;
        for id in &to_delete {
            removed += conn.execute(
                "DELETE FROM workflow_templates WHERE id=?1",
                rusqlite::params![id],
            )?;
        }
        Ok(removed)
    }

    /// Rows whose metadata is missing/broken and should be re-parsed from
    /// disk (failed mid-write scans, pre-v8 rows, pre-lenient-parser rows).
    pub fn list_images_needing_reparse(&self, source_id: i64) -> Result<Vec<(i64, String)>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, abs_path FROM images
             WHERE source_id=?1 AND (meta_ok=0 OR workflow_key IS NULL OR workflow_key='')",
        )?;
        let rows = stmt.query_map(rusqlite::params![source_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Distinct workflow keys without a template assignment yet, for the
    /// one-pass template matching step.
    pub fn distinct_unmatched_workflow_keys(&self) -> Result<Vec<(String, String)>> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT workflow_key, workflow_graph_json FROM images
             WHERE workflow_template_id IS NULL AND workflow_key IS NOT NULL AND workflow_key!=''
               AND workflow_graph_json IS NOT NULL AND workflow_graph_json!=''
             GROUP BY workflow_key",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn assign_workflow_template(&self, workflow_key: &str, template_id: i64, confidence: f64) -> Result<usize> {
        let conn = self.0.lock().unwrap();
        let n = conn.execute(
            "UPDATE images SET workflow_template_id=?2, workflow_match_confidence=?3
             WHERE workflow_key=?1 AND workflow_template_id IS NULL",
            rusqlite::params![workflow_key, template_id, confidence],
        )?;
        Ok(n)
    }

    fn key_col(level: u8) -> &'static str {
        match level {
            0 => "group_key_l0",
            1 => "group_key_l1",
            2 => "group_key_l2",
            _ => "group_key_l3",
        }
    }

    /// All image ids currently holding any of `from_keys` at `level`.
    pub fn image_ids_for_keys(&self, level: u8, from_keys: &[String]) -> Result<Vec<i64>> {
        let conn = self.0.lock().unwrap();
        let col = Self::key_col(level);
        let mut out = Vec::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT id FROM images WHERE {col}=?1 ORDER BY id",
            col = col
        ))?;
        for k in from_keys {
            let rows = stmt.query_map(rusqlite::params![k], |r| r.get::<_, i64>(0))?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    }

    /// (image_id, prompt_pos) for a set of ids — used to derive a canonical
    /// manual group key from the union of member prompts.
    pub fn prompts_for_ids(&self, ids: &[i64]) -> Result<Vec<(i64, String)>> {
        let conn = self.0.lock().unwrap();
        let mut out = Vec::new();
        let mut stmt = conn.prepare("SELECT id, prompt_pos FROM images WHERE id=?1")?;
        for id in ids {
            if let Some((i, p)) = stmt
                .query_row(rusqlite::params![id], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })
                .optional()?
            {
                out.push((i, p));
            }
        }
        Ok(out)
    }

    /// Pin each image to a fixed group key at `level` and rewrite its group
    /// column so display is immediately consistent. Used by manual merge
    /// (every member pinned to the canonical key) and manual split (only
    /// the pulled-out images pinned). Returns the number of images moved.
    pub fn pin_images(&self, level: u8, ids: &[i64], group_key: &str, kind: &str) -> Result<usize> {
        let conn = self.0.lock().unwrap();
        let col = Self::key_col(level);
        let mut moved = 0usize;
        for id in ids {
            conn.execute(
                "INSERT INTO manual_group_bindings (level, image_id, group_key, kind, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(level, image_id) DO UPDATE SET
                    group_key=excluded.group_key, kind=excluded.kind",
                rusqlite::params![level as i64, id, group_key, kind, Utc::now().to_rfc3339()],
            )?;
            moved += conn.execute(
                &format!("UPDATE images SET {col}=?1 WHERE id=?2", col = col),
                rusqlite::params![group_key, id],
            )?;
        }
        Ok(moved)
    }

    /// Whether any image in a group is pinned by a manual binding at `level`.
    fn group_has_binding(&self, conn: &Connection, level: u8, group_key: &str) -> Result<bool> {
        let col = Self::key_col(level);
        let n: i64 = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM manual_group_bindings b
                 JOIN images i ON i.id=b.image_id
                 WHERE b.level=?1 AND i.{col}=?2 AND i.hidden=0",
                col = col
            ),
            rusqlite::params![level as i64, group_key],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }
}

#[derive(Debug, Clone)]
pub struct ImageInsert {
    pub source_id: i64,
    pub rel_path: String,
    pub abs_path: String,
    pub filename: String,
    pub size: i64,
    pub width: i64,
    pub height: i64,
    pub prompt_pos: String,
    pub prompt_neg: String,
    pub checkpoint: String,
    pub loras: String,
    pub vae: String,
    pub samplers: String,
    pub seed: i64,
    pub steps: i64,
    pub cfg: f64,
    pub group_key_l0: String,
    pub group_key_l1: String,
    pub group_key_l2: String,
    pub group_key_l3: String,
    pub sha256: String,
    pub meta_ok: bool,
    pub source_kind: String,
    pub diffusion_model: String,
    pub model_chain_json: String,
    pub workflow_key: String,
    pub workflow_graph_json: String,
    pub workflow_template_id: Option<i64>,
    pub workflow_match_confidence: Option<f64>,
    pub generation_recipe_json: String,
    pub recipe_signature: String,
    pub parser_version: String,
}

#[allow(dead_code)]
pub fn unused_pathbuf() -> PathBuf {
    PathBuf::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_on_fresh_db() {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, migrations::CURRENT_VERSION);
        for col in [
            "diffusion_model",
            "model_chain_json",
            "workflow_key",
            "workflow_graph_json",
            "workflow_template_id",
            "workflow_match_confidence",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('images') WHERE name=?1",
                    rusqlite::params![col],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "images.{col} must exist after migration");
        }
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workflow_templates'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let src_th: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sources') WHERE name='l2_threshold'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(src_th, 1, "sources.l2_threshold must exist after migration");
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        migrations::run(&conn).unwrap();
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, migrations::CURRENT_VERSION);
    }

}
