use crate::db::Db;
use std::collections::HashSet;
use xxhash_rust::xxh3::xxh3_64;

/// Default Jaccard similarity threshold for L2 single-linkage clustering,
/// mirroring the value persisted in the frontend store (l2Threshold).
/// Runtime callers pass the user-adjusted threshold explicitly so the
/// Settings slider can re-cluster live without a re-scan.
pub const DEFAULT_L2_THRESHOLD: f64 = 0.3;

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
/// `threshold` is the Jaccard similarity at/above which two prompts merge.
///
/// Implementation: tokenize once, evaluate every pair exactly once
/// (O(n²) Jaccard in a single pass), and union pairs above the threshold
/// with union-find. Equivalent to the previous iterative "merge best pair
/// and rescan" single-linkage but O(n²) total instead of O(passes · n²),
/// which made every 8s sync freeze the app on 100+ member groups.
pub fn cluster_l1(items: &[(i64, String)], threshold: f64) -> Vec<(i64, u64)> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    // Pre-tokenize once.
    let tokenized: Vec<Vec<String>> = items.iter().map(|(_, p)| tokenize(p)).collect();
    // Union-find with path compression.
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if find(&mut parent, i) == find(&mut parent, j) {
                continue;
            }
            if jaccard(&tokenized[i], &tokenized[j]) >= threshold {
                let ri = find(&mut parent, i);
                let rj = find(&mut parent, j);
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    // Compact cluster ids to 0..k in order of first appearance.
    let mut id_of_root: Vec<u64> = vec![0; n];
    let mut next_id: u64 = 0;
    for i in 0..n {
        let root = find(&mut parent, i);
        if id_of_root[root] == 0 {
            // reserve: first root gets id 0
            next_id += 1;
            id_of_root[root] = next_id;
        }
    }
    items
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, id_of_root[find(&mut parent, i)] - 1))
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

/// Deterministic group key for a MANUAL group (merge / split). Derived only
/// from the sorted set of normalized prompt hashes — independent of L1 and
/// of membership ordering — and namespaced so it can never collide with an
/// auto-computed `cluster_key`. Re-using the same formula on the same
/// prompts yields the same key, so identical prompts naturally re-join the
/// manual group even without an explicit binding.
pub fn manual_key(members: &[(i64, String)]) -> String {
    let mut prompt_hashes: Vec<u64> = members
        .iter()
        .map(|(_, p)| xxh3_64(crate::grouper::normalize_prompt(p).as_bytes()))
        .collect();
    prompt_hashes.sort();
    prompt_hashes.dedup();
    let mut buf = String::from("manual:");
    for h in prompt_hashes {
        buf.push_str(&format!("{:016x}", h));
    }
    format!("{:016x}", xxh3_64(buf.as_bytes()))
}

/// Re-cluster L2 for a given source. Reads all images grouped by L1,
/// clusters each L1's prompts via Jaccard at the given threshold, writes
/// stable cluster keys. The threshold is propagated by the caller so the
/// Settings slider can re-cluster live without a re-scan.
pub fn recluster_l2(db: &Db, source_id: i64, threshold: f64) -> anyhow::Result<()> {
    let keys = HashSet::new();
    recluster_l2_keys(db, source_id, &keys, threshold)
}

/// Incremental variant of `recluster_l2`: only L1 groups listed in `only`
/// are re-clustered. An empty set means "all groups" (full pass).
pub fn recluster_l2_keys(
    db: &Db,
    source_id: i64,
    only: &HashSet<String>,
    threshold: f64,
) -> anyhow::Result<()> {
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
    // Manual group overrides (level 2): images pinned to a fixed key must
    // keep it regardless of what auto-clustering computes — this is what
    // makes merges/splits survive re-clustering. Queried on the SAME
    // connection we already hold (calling a Db method here would re-lock
    // the Mutex and deadlock).
    let mut binding_of: std::collections::HashMap<i64, String> =
        std::collections::HashMap::new();
    {
        let mut bstmt = conn.prepare(
            "SELECT b.image_id, b.group_key
             FROM manual_group_bindings b JOIN images i ON i.id=b.image_id
             WHERE b.level=?1 AND i.source_id=?2",
        )?;
        let brows = bstmt.query_map(rusqlite::params![2, source_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for r in brows {
            let (id, key) = r?;
            binding_of.insert(id, key);
        }
    }

    // Group by L1.
    let mut by_l1: std::collections::HashMap<String, Vec<(i64, String)>> =
        std::collections::HashMap::new();
    for (id, prompt, l1) in all {
        by_l1.entry(l1).or_default().push((id, prompt));
    }

    let mut updates: Vec<(i64, String)> = Vec::new();
    for (l1, members) in by_l1.iter() {
        if !only.is_empty() && !only.contains(l1) {
            continue;
        }
        let clusters = cluster_l1(members, threshold);
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
            for (id, _) in &mems {
                // A pinned image wins over the auto-computed key. Pinned
                // images with the same binding key stay together; a split
                // image with a different binding key separates even if the
                // auto-cluster would have merged it back.
                let final_key = binding_of.get(id).cloned().unwrap_or_else(|| key.clone());
                updates.push((*id, final_key));
            }
        }
    }

    if updates.is_empty() {
        return Ok(());
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_splits_distinct_prompts() {
        let items = vec![
            (1i64, "masterpiece, a girl in red".to_string()),
            (2i64, "masterpiece, a girl in red".to_string()),
            (3i64, "a dragon over a castle".to_string()),
        ];
        let out = cluster_l1(&items, 0.3);
        let c1 = out.iter().find(|(id, _)| *id == 1).unwrap().1;
        let c2 = out.iter().find(|(id, _)| *id == 2).unwrap().1;
        let c3 = out.iter().find(|(id, _)| *id == 3).unwrap().1;
        assert_eq!(c1, c2);
        assert_ne!(c1, c3);
    }

    #[test]
    fn chain_merges_single_linkage() {
        // 1~2 and 2~3 similar at threshold, 1~3 disjoint: all three merge.
        let items = vec![
            (1i64, "alpha beta".to_string()),
            (2i64, "beta gamma".to_string()),
            (3i64, "gamma delta".to_string()),
        ];
        let out = cluster_l1(&items, 0.33);
        let c1 = out.iter().find(|(id, _)| *id == 1).unwrap().1;
        let c2 = out.iter().find(|(id, _)| *id == 2).unwrap().1;
        let c3 = out.iter().find(|(id, _)| *id == 3).unwrap().1;
        assert_eq!(c1, c2);
        assert_eq!(c2, c3);
    }

    #[test]
    fn empty_input() {
        assert!(cluster_l1(&[], 0.3).is_empty());
    }
}
