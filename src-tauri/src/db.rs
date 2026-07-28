use anyhow::Result;
use chrono::Utc;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Db(pub Mutex<Connection>);

pub fn open(path: &std::path::Path) -> Result<Db> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    conn.execute_batch("PRAGMA busy_timeout=5000;")?;
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
    /// filesystem removals during a rescan.
    pub fn delete_missing_source_images(
        &self,
        source_id: i64,
        keep: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        let conn = self.0.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, abs_path FROM images WHERE source_id=?1")?;
        let rows = stmt.query_map(rusqlite::params![source_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut to_delete: Vec<i64> = Vec::new();
        for r in rows {
            let (id, p) = r?;
            if !keep.contains(&p) {
                to_delete.push(id);
            }
        }
        drop(stmt);
        let mut removed = 0usize;
        for id in &to_delete {
            removed += conn.execute(
                "DELETE FROM images WHERE id=?1",
                rusqlite::params![id],
            )?;
        }
        Ok(removed)
    }
}

mod migrations {
    use super::*;

    const CURRENT_VERSION: i32 = 3;

    pub fn run(conn: &Connection) -> Result<()> {
        let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // Never drop — all existing data (scores/labels/scan cache) must
        // survive across launches and migrations. `CREATE TABLE IF NOT
        // EXISTS` keeps the schema current; future column additions go via
        // guarded `ALTER TABLE ... ADD COLUMN`.
        create_schema(conn)?;
        seed_default_labels(conn)?;
        let _ = version;
        conn.execute_batch(&format!("PRAGMA user_version = {};", CURRENT_VERSION))?;
        Ok(())
    }

    fn create_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sources (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                alias TEXT,
                scanned_at TEXT
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
            ",
        )?;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRow {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub alias: Option<String>,
    pub scanned_at: Option<String>,
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
            "SELECT id, path, kind, alias, scanned_at FROM sources ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                path: r.get(1)?,
                kind: r.get(2)?,
                alias: r.get(3)?,
                scanned_at: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn clear_source_images(&self, source_id: i64) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "DELETE FROM images WHERE source_id=?1",
            rusqlite::params![source_id],
        )?;
        Ok(())
    }

    pub fn insert_image(&self, row: &ImageInsert) -> Result<i64> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO images (
                source_id, rel_path, abs_path, filename, size, width, height,
                prompt_pos, prompt_neg, checkpoint, loras, vae, samplers, seed, steps, cfg,
                group_key_l0, group_key_l1, group_key_l2, group_key_l3,
                sha256, meta_ok, source_kind, scanned_at
            ) VALUES (
                ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                ?17,?18,?19,?20,?21,?22,?23,?24
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
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_groups(&self, source_id: Option<i64>, level: u8) -> Result<Vec<GroupInfo>> {
        let conn = self.0.lock().unwrap();
        let key_col = match level {
            0 => "group_key_l0",
            1 => "group_key_l1",
            2 => "group_key_l2",
            _ => "group_key_l3",
        };
        let sql = match source_id {
            Some(sid) => format!(
                "SELECT {col} AS gk, COUNT(*) c, MIN(prompt_pos) pp, MIN(checkpoint) ck, MIN(source_kind) sk, MIN(s.path) sp
                 FROM images i JOIN sources s ON s.id=i.source_id
                 WHERE i.source_id={sid} AND {col} IS NOT NULL
                 GROUP BY {col} ORDER BY c DESC",
                col = key_col, sid = sid
            ),
            None => format!(
                "SELECT {col} AS gk, COUNT(*) c, MIN(prompt_pos) pp, MIN(checkpoint) ck, MIN(source_kind) sk, MIN(s.path) sp
                 FROM images i JOIN sources s ON s.id=i.source_id
                 WHERE {col} IS NOT NULL
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
            })
        };
        let mut out = Vec::new();
        if source_id.is_some() {
            let rows = stmt.query_map([], map)?;
            for r in rows {
                out.push(r?);
            }
        } else {
            let rows = stmt.query_map([], map)?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    }

    pub fn list_group_images(&self, group_key: &str, level: u8) -> Result<Vec<ImageRow>> {
        let conn = self.0.lock().unwrap();
        let key_col = match level {
            0 => "group_key_l0",
            1 => "group_key_l1",
            2 => "group_key_l2",
            _ => "group_key_l3",
        };
        let sql = format!(
            "SELECT id, source_id, abs_path, filename, width, height, prompt_pos, prompt_neg,
                    checkpoint, loras, vae, samplers, seed, meta_ok, source_kind
             FROM images WHERE {col}=?1 ORDER BY seed, filename",
            col = key_col
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

    pub fn set_score(&self, image_id: i64, score: f64, mode: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO scores (image_id, internal_score, updated_at, last_mode)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(image_id) DO UPDATE SET internal_score=excluded.internal_score, updated_at=excluded.updated_at, last_mode=excluded.last_mode",
            rusqlite::params![image_id, score, Utc::now().to_rfc3339(), mode],
        )?;
        Ok(())
    }

    pub fn add_compare_pair(
        &self,
        group_key: &str,
        left: i64,
        right: i64,
        winner: i64,
        delta: f64,
    ) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT INTO compare_pairs (group_key, left_img, right_img, winner, delta, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![group_key, left, right, winner, delta, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn record_archive(&self, image_id: i64, dest: &str) -> Result<()> {
        let conn = self.0.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO archive_log (image_id, dest_path, archived_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![image_id, dest, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn is_archived(&self, image_id: i64) -> Result<bool> {
        let conn = self.0.lock().unwrap();
        let c: i64 = conn.query_row(
            "SELECT COUNT(*) FROM archive_log WHERE image_id=?1",
            rusqlite::params![image_id],
            |r| r.get(0),
        )?;
        Ok(c > 0)
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
}

#[allow(dead_code)]
pub fn unused_pathbuf() -> PathBuf {
    PathBuf::new()
}
