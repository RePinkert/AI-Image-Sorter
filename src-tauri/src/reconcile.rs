//! 库恢复：从**另一个库文件**里把用户的手工分拣结果找回来。
//!
//! ## 为什么需要这个模块（第 29 轮）
//!
//! 图片在库里的身份是**路径**。用户在应用之外改名 → 重扫时旧行被删、
//! 新行被插 → `image_id` 变了 → 评分 / 标签 / 手动分组全部消失。
//!
//! **但旧状态往往还在**：换了发行方式（安装版 → 便携版）、或换了应用数据
//! 目录之后，老的库文件就成了「没人用的副本」 —— 它里面还留着改名前的
//! 手工分拣结果。
//!
//! 这个模块的能力：拿两个库，按**内容身份**把老库的行认领到新库的行上，
//! 把「评分 / 标签 / 手动分组」搬回来，并如实报告搬回来多少、依据是什么。
//!
//! ## 三条口径
//!
//! 1. **只按内容身份匹配**，不用路径（路径正是出问题的东西）。
//! 2. **新库已有的优先** —— 冲突时不动新库的值（用户后做的决定算数）。
//! 3. **组键要重映射** —— 老库的组键在新库里多半已经不存在了；按「同一个
//!    老组的成员现在都在哪个新组」取多数派，把老断言落到那个新组上。
//!
//! 标记串住在本模块 —— 实现被删 → 标记串消失 → 发行版探针立刻发现
//! （见 `ui-program.md` §产物可证）。

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::db::Db;
use crate::fingerprint::{self, RenameCandidate, RenameOld};

/// 能力标记串：库恢复（第 29 轮）。
pub const CAP_LIBRARY_RECOVER: &str = "ais.cap.library.recover.v1\0";

/// 老库里的一行（只取匹配要用的字段）。
struct OldRow {
    id: i64,
    size: i64,
    seed: i64,
    prompt_pos: String,
    prompt_neg: String,
    meta_sig: String,
    content_fp: String,
}

/// 一条要搬回来的手动分组断言。
pub struct RestoreBinding {
    new_image_id: i64,
    group_key: String,
    kind: String,
    prev_group_key: Option<String>,
}

/// 恢复计划（预览与执行共用同一份计算 —— 免得「预览的数字」和
/// 「实际恢复的数字」对不上）。
pub struct Plan {
    /// (老库 image_id, 新库 image_id)。
    pub matches: Vec<(i64, i64)>,
    /// 匹配依据的分布（按依据分组计数）。
    pub basis_counts: Vec<(String, usize)>,
    pub bindings: Vec<RestoreBinding>,
    pub labels: Vec<(i64, String, String, Option<String>)>,
    pub scores: Vec<(i64, f64)>,
    /// 老库里没能认领到新库的行数。
    pub unmatched_old: usize,
    /// 老库里的行总数。
    pub old_images: usize,
    /// 因为组键无法重映射而跳过的条数。
    pub skipped_unmappable: usize,
}

/// 预览（用户点「预览」时看到的数字）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Preview {
    pub path: String,
    pub old_images: usize,
    pub matched: usize,
    pub unmatched_old: usize,
    pub bindings: usize,
    pub labels: usize,
    pub scores: usize,
    pub skipped_conflict: usize,
    pub skipped_unmappable: usize,
    pub basis: Vec<(String, usize)>,
}

/// 执行结果（用户点「找回」后看到的数字）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Outcome {
    pub path: String,
    pub matched: usize,
    pub bindings: usize,
    pub labels: usize,
    pub scores: usize,
    pub skipped_conflict: usize,
    pub skipped_unmappable: usize,
    pub basis: Vec<(String, usize)>,
}

/// 打开另一个库（**只读** —— 绝不能改用户的旧库）。
fn open_readonly(path: &str) -> Result<Connection, String> {
    if !Path::new(path).is_file() {
        return Err(format!("找不到库文件：{path}"));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开旧库失败：{e}"))
}

/// 读老库的图片行。
///
/// **老库的 schema 可能更老**（比如没有 `content_fp`）—— 所以先看有哪些列，
/// 再按实际有的列拼查询。缺列就当默认值（那只会让匹配退到更弱的依据上，
/// 不会让它出错）。
fn read_old_images(conn: &Connection) -> Result<Vec<OldRow>, String> {
    let mut cols: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('images')")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for r in rows {
            cols.insert(r.map_err(|e| e.to_string())?);
        }
    }
    // 每个字段：列名 + 没有该列时的默认值。
    let pick = |name: &str, fallback: &str| -> String {
        if cols.contains(name) {
            format!("COALESCE({name},{fallback})")
        } else {
            fallback.to_string()
        }
    };
    let sql = format!(
        "SELECT id, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}
         FROM images ORDER BY id",
        pick("size", "0"),
        pick("seed", "0"),
        pick("prompt_pos", "''"),
        pick("prompt_neg", "''"),
        pick("workflow_key", "''"),
        pick("width", "0"),
        pick("height", "0"),
        pick("steps", "0"),
        pick("cfg", "0.0"),
        pick("recipe_signature", "''"),
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let width = r.get::<_, i64>(6)?;
            let height = r.get::<_, i64>(7)?;
            Ok(OldRow {
                id: r.get::<_, i64>(0)?,
                size: r.get::<_, i64>(1)?,
                seed: r.get::<_, i64>(2)?,
                prompt_pos: r.get::<_, String>(3)?,
                prompt_neg: r.get::<_, String>(4)?,
                meta_sig: fingerprint::meta_signature(
                    &r.get::<_, String>(5)?,
                    width,
                    height,
                    r.get::<_, i64>(8)?,
                    r.get::<_, f64>(9)?,
                    &r.get::<_, String>(10)?,
                ),
                content_fp: String::new(),
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// 计算恢复计划。
pub fn build_plan(db: &Db, other_path: &str) -> Result<Plan, String> {
    let conn = open_readonly(other_path)?;
    let old_rows = read_old_images(&conn)?;
    let new_rows = db.list_all_identity_rows().map_err(|e| e.to_string())?;

    let olds: Vec<RenameOld> = old_rows
        .iter()
        .map(|r| RenameOld {
            id: r.id,
            abs_path: String::new(),
            size: r.size,
            seed: r.seed,
            prompt_pos: r.prompt_pos.clone(),
            prompt_neg: r.prompt_neg.clone(),
            meta_sig: r.meta_sig.clone(),
            content_fp: r.content_fp.clone(),
        })
        .collect();
    let news: Vec<RenameCandidate> = new_rows
        .iter()
        .map(|r| RenameCandidate {
            size: r.size,
            seed: r.seed,
            prompt_pos: r.prompt_pos.clone(),
            prompt_neg: r.prompt_neg.clone(),
            meta_sig: r.meta_sig(),
            content_fp: r.content_fp.clone(),
        })
        .collect();

    let matched = fingerprint::match_renames(&olds, &news);
    let mut basis_counts: HashMap<String, usize> = HashMap::new();
    let mut old_to_new: HashMap<i64, i64> = HashMap::new();
    for m in &matched {
        *basis_counts
            .entry(m.basis.as_str().to_string())
            .or_insert(0) += 1;
        old_to_new.insert(olds[m.old_index].id, new_rows[m.new_index].id);
    }
    let mut basis: Vec<(String, usize)> = basis_counts.into_iter().collect();
    basis.sort_by(|a, b| b.1.cmp(&a.1));

    // ── 手动分组断言 ────────────────────────────────────────────
    // 老库里「哪个组有哪些成员」—— 组键重映射要用。
    let mut old_group_members: HashMap<String, Vec<i64>> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT group_key, image_id FROM manual_group_bindings WHERE level=2")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (key, image_id) = r.map_err(|e| e.to_string())?;
            old_group_members.entry(key).or_default().push(image_id);
        }
    }
    // 老组键 → 新组键（取多数派）。
    let new_group_of: HashMap<i64, String> = db
        .list_image_group_keys()
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();
    let mut key_remap: HashMap<String, String> = HashMap::new();
    for (old_key, members) in &old_group_members {
        let mut tally: HashMap<String, usize> = HashMap::new();
        for m in members {
            if let Some(new_id) = old_to_new.get(m) {
                if let Some(g) = new_group_of.get(new_id) {
                    *tally.entry(g.clone()).or_insert(0) += 1;
                }
            }
        }
        if let Some((best, _)) = tally.into_iter().max_by_key(|(_, c)| *c) {
            key_remap.insert(old_key.clone(), best);
        }
    }

    let mut bindings: Vec<RestoreBinding> = Vec::new();
    let mut skipped_unmappable = 0usize;
    {
        let mut stmt = conn
            .prepare(
                "SELECT image_id, group_key, kind, COALESCE(prev_group_key,'') \
                 FROM manual_group_bindings WHERE level=2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (old_id, group_key, kind, prev) = r.map_err(|e| e.to_string())?;
            let Some(new_id) = old_to_new.get(&old_id).copied() else {
                continue;
            };
            let Some(mapped) = key_remap.get(&group_key) else {
                skipped_unmappable += 1;
                continue;
            };
            let mapped_prev = if prev.is_empty() {
                None
            } else {
                key_remap.get(&prev).cloned()
            };
            bindings.push(RestoreBinding {
                new_image_id: new_id,
                group_key: mapped.clone(),
                kind,
                prev_group_key: mapped_prev,
            });
        }
    }

    // ── 标签 ────────────────────────────────────────────────────
    // 按**名字**搬（两边的 label_id 未必一致），连着手势与颜色一起搬。
    let mut labels: Vec<(i64, String, String, Option<String>)> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT l.image_id, lb.name, COALESCE(lb.gesture,''), lb.color \
                 FROM image_labels l JOIN labels lb ON lb.id=l.label_id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (old_id, name, gesture, color) = r.map_err(|e| e.to_string())?;
            if let Some(new_id) = old_to_new.get(&old_id) {
                labels.push((*new_id, name, gesture, color));
            }
        }
    }

    // ── 评分 ────────────────────────────────────────────────────
    let mut scores: Vec<(i64, f64)> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT image_id, internal_score FROM scores")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))
            .map_err(|e| e.to_string())?;
        for r in rows {
            let (old_id, score) = r.map_err(|e| e.to_string())?;
            if let Some(new_id) = old_to_new.get(&old_id) {
                scores.push((*new_id, score));
            }
        }
    }

    let matched_count = old_to_new.len();
    Ok(Plan {
        matches: old_to_new.into_iter().collect(),
        basis_counts: basis,
        bindings,
        labels,
        scores,
        unmatched_old: old_rows.len().saturating_sub(matched_count),
        old_images: old_rows.len(),
        skipped_unmappable,
    })
}

/// 预览。
pub fn preview(db: &Db, other_path: &str) -> Result<Preview, String> {
    let plan = build_plan(db, other_path)?;
    // 「新库已有就跳过」要在预览里就扣掉 —— 预览数字必须等于执行后的数字。
    let mut skipped_conflict = 0usize;
    let mut bindings = 0usize;
    let mut labels = 0usize;
    let mut scores = 0usize;
    for b in &plan.bindings {
        if db.has_binding(b.new_image_id).map_err(|e| e.to_string())? {
            skipped_conflict += 1;
        } else {
            bindings += 1;
        }
    }
    for (image_id, name, _, _) in &plan.labels {
        if db
            .image_has_label(*image_id, name)
            .map_err(|e| e.to_string())?
        {
            skipped_conflict += 1;
        } else {
            labels += 1;
        }
    }
    for (image_id, _) in &plan.scores {
        if db.score_of(*image_id).map_err(|e| e.to_string())?.is_some() {
            skipped_conflict += 1;
        } else {
            scores += 1;
        }
    }
    Ok(Preview {
        path: other_path.to_string(),
        old_images: plan.old_images,
        matched: plan.matches.len(),
        unmatched_old: plan.unmatched_old,
        bindings,
        labels,
        scores,
        skipped_conflict,
        skipped_unmappable: plan.skipped_unmappable,
        basis: plan.basis_counts.clone(),
    })
}

/// 执行恢复。
pub fn apply(db: &Db, other_path: &str) -> Result<Outcome, String> {
    let plan = build_plan(db, other_path)?;
    let mut bindings = 0usize;
    let mut labels = 0usize;
    let mut scores = 0usize;
    let mut skipped_conflict = 0usize;

    // 标签：按**名字**映射（两边的 label_id 未必一致）。
    let existing_labels = db.list_labels().map_err(|e| e.to_string())?;
    for (image_id, name, gesture, color) in &plan.labels {
        if db
            .image_has_label(*image_id, name)
            .map_err(|e| e.to_string())?
        {
            skipped_conflict += 1;
            continue;
        }
        let label_id = match existing_labels.iter().find(|l| &l.name == name) {
            Some(l) => l.id,
            None => db
                .upsert_label(None, name, gesture, color.as_deref())
                .map_err(|e| e.to_string())?,
        };
        db.set_image_label(*image_id, label_id, true)
            .map_err(|e| e.to_string())?;
        labels += 1;
    }

    for (image_id, score) in &plan.scores {
        if db.score_of(*image_id).map_err(|e| e.to_string())?.is_some() {
            skipped_conflict += 1;
            continue;
        }
        db.set_score(*image_id, *score).map_err(|e| e.to_string())?;
        scores += 1;
    }

    for b in &plan.bindings {
        if db.has_binding(b.new_image_id).map_err(|e| e.to_string())? {
            skipped_conflict += 1;
            continue;
        }
        let inserted = db
            .insert_binding_if_absent(
                2,
                b.new_image_id,
                &b.group_key,
                &b.kind,
                b.prev_group_key.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        if inserted {
            // 断言要立刻生效 —— 否则「找回来了」但列表上看不出来。
            db.set_image_group_key_l2(b.new_image_id, &b.group_key)
                .map_err(|e| e.to_string())?;
            bindings += 1;
        } else {
            skipped_conflict += 1;
        }
    }

    Ok(Outcome {
        path: other_path.to_string(),
        matched: plan.matches.len(),
        bindings,
        labels,
        scores,
        skipped_conflict,
        skipped_unmappable: plan.skipped_unmappable,
        basis: plan.basis_counts.clone(),
    })
}

/// 列出**可能**是「旧副本」的库文件。
///
/// 口径：同一个应用数据目录下的同名库（安装版 vs 便携版换目录时最常见的
/// 情况），以及**当前库所在目录的兄弟文件**。用户也可以自己指定路径。
pub fn candidate_libraries(current_db_path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: String| {
        if Path::new(&p).is_file() && !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(dir) = Path::new(current_db_path).parent() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let path = e.path();
                if path.extension().and_then(|s| s.to_str()) == Some("db")
                    && path.to_string_lossy() != current_db_path
                {
                    push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    out
}

/// 库恢复的对外能力名（设置页与探针用）。
pub const RECOVER_ACTION: &str = "从旧库找回分拣结果";
