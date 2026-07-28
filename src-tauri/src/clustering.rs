use crate::db::Db;
use xxhash_rust::xxh3::xxh3_64;

/// Jaccard similarity threshold for L2 single-linkage clustering.
/// Hardcoded for now; a Settings slider can be added later.
pub const L2_SIMILARITY_THRESHOLD: f64 = 0.5;

/// Tokenize a prompt for Jaccard similarity.
/// Normalizes whitespace + lowercase, splits on commas and whitespace,
/// keeps CJK characters as per-character tokens (since CJK has no spaces),
/// and drops empty / single-char ASCII tokens.
pub fn tokenize(prompt: &str) -> Vec<String> {
    let lower = prompt.to_lowercase();
    let mut tokens: Vec<String> = Vec::new();
    for part in lower.split(|c: char| c == ',' || c.is_whitespace()) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // If the part is purely ASCII / latin, split further on whitespace is
        // already done; keep whole-word tokens of length >= 1.
        let has_cjk = part.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c));
        if has_cjk {
            // Per-character CJK tokens; group consecutive non-CJK runs as one.
            let mut buf = String::new();
            for c in part.chars() {
                if ('\u{4E00}'..='\u{9FFF}').contains(&c) {
                    if !buf.is_empty() {
                        tokens.push(std::mem::take(&mut buf));
                    }
                    tokens.push(c.to_string());
                } else if c.is_alphanumeric() || c == '\'' || c == '-' {
                    buf.push(c);
                } else if !buf.is_empty() {
                    tokens.push(std::mem::take(&mut buf));
                }
            }
            if !buf.is_empty() {
                tokens.push(buf);
            }
        } else {
            tokens.push(part.to_string());
        }
    }
    tokens
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let sa: std::collections::HashSet<&String> = a.iter().collect();
    let sb: std::collections::HashSet<&String> = b.iter().collect();
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        return 1.0;
    }
    inter / union
}

/// Single-linkage agglomerative clustering of `(image_id, prompt)` pairs.
/// Returns `(image_id, cluster_index)` for each input, in input order.
pub fn cluster_l1(items: &[(i64, String)]) -> Vec<(i64, u64)> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    // Pre-tokenize once.
    let tokenized: Vec<Vec<String>> = items.iter().map(|(_, p)| tokenize(p)).collect();
    // Each item starts as its own cluster.
    let mut cluster_of: Vec<usize> = (0..n).collect();
    // Greedy single-linkage merges: repeatedly merge the closest pair >= T.
    loop {
        let mut best: Option<(usize, usize, f64)> = None;
        for i in 0..n {
            for j in (i + 1)..n {
                if cluster_of[i] == cluster_of[j] {
                    continue;
                }
                let sim = jaccard(&tokenized[i], &tokenized[j]);
                if sim >= L2_SIMILARITY_THRESHOLD {
                    match best {
                        Some((_, _, bs)) if bs >= sim => {}
                        _ => best = Some((i, j, sim)),
                    }
                }
            }
        }
        match best {
            Some((i, j, _)) => {
                let src = cluster_of[i];
                let dst = cluster_of[j];
                let target = src.min(dst);
                for c in cluster_of.iter_mut() {
                    if *c == src || *c == dst {
                        *c = target;
                    }
                }
            }
            None => break,
        }
    }
    // Compact cluster ids to 0..k.
    let mut ids: Vec<usize> = cluster_of.clone();
    ids.sort();
    ids.dedup();
    let mut compact_index: Vec<u64> = Vec::with_capacity(n);
    compact_index.resize(*ids.last().unwrap_or(&0) + 1, 0);
    for (idx, c) in ids.iter().enumerate() {
        compact_index[*c] = idx as u64;
    }
    items
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, compact_index[cluster_of[i]]))
        .collect()
}

/// Compute a stable L2 group key for a cluster: independent of image_id
/// so a re-scan that adds a prompt-identical image keeps the same key.
/// key = xxh3( l1_hex || 0x02 || sorted(unique prompt_pos hashes).join(",") )
pub fn cluster_key(l1: &str, members: &[(i64, String)]) -> String {
    let mut prompt_hashes: Vec<u64> = members
        .iter()
        .map(|(_, p)| xxh3_64(crate::grouper::normalize_prompt(p).as_bytes()))
        .collect();
    prompt_hashes.sort();
    prompt_hashes.dedup();
    let mut buf = String::new();
    buf.push_str(l1);
    buf.push('\x02');
    let joined = prompt_hashes
        .iter()
        .map(|h| format!("{:016x}", h))
        .collect::<Vec<_>>()
        .join(",");
    buf.push_str(&joined);
    format!("{:016x}", xxh3_64(buf.as_bytes()))
}

/// Re-cluster L2 for a given source. Reads all images grouped by L1,
/// clusters each L1's prompts via Jaccard, writes stable cluster keys.
pub fn recluster_l2(db: &Db, source_id: i64) -> anyhow::Result<()> {
    let conn = db.0.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, prompt_pos, group_key_l1 FROM images
         WHERE source_id=?1 ORDER BY id",
    )?;
    let rows = stmt.query_map(rusqlite::params![source_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut all: Vec<(i64, String, String)> = Vec::new();
    for r in rows {
        all.push(r?);
    }
    drop(stmt);

    // Group by L1.
    let mut by_l1: std::collections::HashMap<String, Vec<(i64, String)>> =
        std::collections::HashMap::new();
    for (id, prompt, l1) in all {
        by_l1.entry(l1).or_default().push((id, prompt));
    }

    let mut updates: Vec<(i64, String)> = Vec::new();
    for (l1, members) in by_l1.iter() {
        let clusters = cluster_l1(members);
        // Bucket image_ids per cluster index.
        let mut buckets: std::collections::HashMap<u64, Vec<(i64, String)>> =
            std::collections::HashMap::new();
        for (id, prompt) in members.iter() {
            let cid = clusters
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, c)| *c)
                .unwrap_or(0);
            buckets.entry(cid).or_default().push((*id, prompt.clone()));
        }
        for (_, mems) in buckets {
            if mems.is_empty() {
                continue;
            }
            let key = cluster_key(l1, &mems);
            for (id, _) in mems {
                updates.push((id, key.clone()));
            }
        }
    }

    let tx = conn.unchecked_transaction()?;
    for (id, key) in &updates {
        tx.execute(
            "UPDATE images SET group_key_l2=?1 WHERE id=?2",
            rusqlite::params![key, id],
        )?;
    }
    tx.commit()?;
    Ok(())
}