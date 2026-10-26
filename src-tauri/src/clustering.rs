use crate::db::Db;
use std::collections::HashSet;
use xxhash_rust::xxh3::xxh3_64;

/// 能力标记：L2 词 Jaccard 单链（历史行为）。
///
/// 标记串住在**实现它的这个模块**里 —— 删掉实现，常量消失，标记串随之
/// 从二进制里消失（见 `capabilities.rs` 的纪律说明）。
///
/// 末尾的 `\0` 是 C 风格哨兵：Rust 字面量在 `.rodata` 里不带终止符，
/// 探针靠它把标记串与相邻字面量分开。
pub const CAP_L2_TEXT_JACCARD: &str = "ais.cap.l2.text-jaccard.v1\0";
/// 能力标记：L2 视觉塔平均连接（阶段六）。
pub const CAP_L2_VISUAL: &str = "ais.cap.l2.visual-avg-linkage.v1\0";

/// Default Jaccard similarity threshold for L2 single-linkage clustering,
/// mirroring the value persisted in the frontend store (l2Threshold).
/// Runtime callers pass the user-adjusted threshold explicitly so the
/// Settings slider can re-cluster live without a re-scan.
pub const DEFAULT_L2_THRESHOLD: f64 = 0.3;

/// Default cosine threshold for the visual / semantic L2 modes.
///
/// 这个 0.75 不是拍脑袋：实测（见 ui-log.md 0b-2）在真实语料上，
/// 视觉平均连接 @0.75 的「最近邻一致率」96.0%、「松散对率」0.7%，
/// **追平并超过用户手工合并的结果**（92.0% / 0.7%）；而 @0.7 的松散对率
/// 跳到 6.3%（单链粘连），@0.8 则开始过切（一致率掉到 93%）。
pub const DEFAULT_VISUAL_THRESHOLD: f64 = 0.75;

/// 语义档（BGE）的**暂定**余弦阈值。
///
/// ⚠ **这个值未标定。** 0.75 是 **CLIP 的值**（实测标定出来的），BGE 余弦
/// 的分布与 CLIP 不同 —— 沿用 0.75 就是拿另一个空间的刻度卡这个空间。
///
/// 阶段七轮 3 走双源标定（A 轨合成语料的判别力 + 用户自己的聚类反标定），
/// 标定结果只作**建议**，应用仍走「应用并重新聚类」按钮。
///
/// 在标定完成前，语义档**不得设为默认档**（ui-program.md 阶段七「已定问题」）。
pub const DEFAULT_SEMANTIC_THRESHOLD: f64 = 0.7;

/// L2 的聚类方式。
///
/// `Text` 是历史行为（词 Jaccard 单链）；`Visual` / `Semantic` 是阶段六的
/// 语义聚类：视觉塔给图、文本塔给 prompt 算向量，再用**平均连接**聚合。
///
/// 为什么平均连接：单链会把「A~B、B~C」的两条弱边拉成「A~C」的强边
/// （链式粘连）。研究阶段实测，词 Jaccard 单链在真实语料上有 19.4% 的
/// 同组对低于阈值，140 张图连一条够格的边都没有。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L2Mode {
    /// 词 Jaccard（历史行为；L3 与「本地改 prompt」依赖它的切分口径）
    Text,
    /// 视觉塔向量（图像）
    Visual,
    /// 文本塔向量（prompt 语义）
    Semantic,
}

impl L2Mode {
    /// 全部档位（运行时真值 —— 设置页的下拉与构建能力清单都用它）。
    pub fn all() -> &'static [L2Mode] {
        &[L2Mode::Text, L2Mode::Visual, L2Mode::Semantic]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            L2Mode::Text => "text",
            L2Mode::Visual => "visual",
            L2Mode::Semantic => "semantic",
        }
    }

    pub fn parse(value: &str) -> L2Mode {
        match value {
            "visual" => L2Mode::Visual,
            "semantic" => L2Mode::Semantic,
            _ => L2Mode::Text,
        }
    }

    /// 该模式该读哪一份签名：**槽位 + 模型标识**。
    ///
    /// 这是**唯一的口径来源** —— 读签名、补算签名、重聚类都必须走它。
    ///
    /// 为什么要有这个函数：阶段七轮 2 之前，`run_sync` 的语义模式分支用
    /// **视觉引擎**算向量，却写进 `slot='text'` 的槽里 —— **错空间**。
    /// 后果很隐蔽：槽位里确实有 512 维向量，条数也对，只有**值**是 CLIP 的
    /// 空间（视觉塔是 512 维，文本塔也是 512 维 —— 维度一样，肉眼查不出来）。
    /// 任何一处单独写 `("text", TEXT_MODEL_ID)` 都有可能再犯，所以收口到
    /// 一个函数里，并由单测守着。
    pub fn signature_spec(&self) -> Option<(&'static str, &'static str)> {
        match self {
            L2Mode::Text => None,
            L2Mode::Visual => Some(("vision", crate::vision::VISION_MODEL_ID)),
            L2Mode::Semantic => Some(("text", crate::text::TEXT_MODEL_ID)),
        }
    }

    /// 该模式是不是用余弦阈值（否则是 Jaccard）。
    pub fn uses_cosine(&self) -> bool {
        !matches!(self, L2Mode::Text)
    }
}


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

/// 余弦相似度（向量已 L2 归一化 → 点积即余弦）。
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut sum = 0f32;
    for i in 0..n {
        sum += a[i] * b[i];
    }
    sum
}

/// **平均连接**聚合（average-linkage）。
///
/// 为什么不是单链：单链只要「A~B、B~C」就会把 A、C 拉到一起，哪怕 A、C
/// 自己差得很远（链式粘连）。研究阶段实测，词 Jaccard 单链在真实语料上
/// 有 19.4% 的同组对低于阈值、140 张图连一条够格的边都没有。
/// 平均连接要求**整簇**都达标，粘连从根上消失。
///
/// 簇间相似度 = 成员对相似度的平均；每轮合并平均相似度最高的一对，
/// 低于阈值就停。确定性：没有随机成分，同一份输入永远得到同一份分组。
pub fn average_linkage(items: &[(i64, Vec<f32>)], threshold: f64) -> Vec<u32> {
    let n = items.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0];
    }
    // 相似度矩阵
    let mut sim = vec![0f32; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            let s = cosine(&items[i].1, &items[j].1);
            sim[i * n + j] = s;
            sim[j * n + i] = s;
        }
    }
    let mut clusters: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    while clusters.len() > 1 {
        let mut best = f64::NEG_INFINITY;
        let mut bi = 0usize;
        let mut bj = 1usize;
        for i in 0..clusters.len() {
            for j in (i + 1)..clusters.len() {
                let a = &clusters[i];
                let b = &clusters[j];
                let mut sum = 0f64;
                for &x in a {
                    for &y in b {
                        sum += sim[x * n + y] as f64;
                    }
                }
                let avg = sum / (a.len() * b.len()) as f64;
                if avg > best {
                    best = avg;
                    bi = i;
                    bj = j;
                }
            }
        }
        if best < threshold as f64 {
            break;
        }
        let mut merged = clusters[bi].clone();
        merged.extend(clusters[bj].iter().copied());
        clusters.remove(bj);
        clusters.remove(bi);
        clusters.push(merged);
    }
    let mut labels = vec![0u32; n];
    for (index, members) in clusters.iter().enumerate() {
        for &m in members {
            labels[m] = index as u32;
        }
    }
    labels
}

/// 视觉/语义簇的**稳定**组键。
///
/// 关键性质：簇**长大**时键不能变。文本模式的老做法是「成员集合的哈希」——
/// 每多一个成员键就变一次（组会「换身份」）。这里改用「簇内最小 image_id」
/// 作为身份：成员增加不影响它，成员被移走才会变（那时确实该变）。
///
/// 命名空间 "vec" 与文本键隔离，两种模式的键不会撞。
fn vector_cluster_key(l1: &str, member_ids: &[i64]) -> String {
    let min_id = member_ids.iter().copied().min().unwrap_or(0);
    let mut buf = String::from("vec\x01");
    buf.push_str(l1);
    buf.push('\x03');
    buf.push_str(&min_id.to_string());
    format!("{:016x}", xxh3_64(buf.as_bytes()))
}

/// 按 L1 桶做语义聚类（视觉 / 文本塔向量），写回 `group_key_l2`。
///
/// 手动 pin 优先：被合并/拆分/移动过的图保持自己的组键（与文本模式一致）。
/// 没有向量的图保持原组（不做无声的搬动 —— 用户能看见「哪些图没参与」）。
///
/// `only` 非空时只重算这些 L1 桶（后台同步的增量路径）。
///
/// 返回实际改动的行数。
pub fn recluster_l2_vectors(
    db: &Db,
    source_id: i64,
    slot: &str,
    model_id: &str,
    threshold: f64,
    only: Option<&HashSet<String>>,
) -> anyhow::Result<usize> {
    let images = db.list_images_for_clustering(source_id)?;
    let signatures = db.signatures_for(source_id, slot, model_id)?;
    if signatures.is_empty() {
        return Ok(0);
    }
    let vec_of: std::collections::HashMap<i64, Vec<f32>> = signatures.into_iter().collect();
    let bindings = db.manual_l2_bindings(source_id)?;

    // 按 L1 桶分组（与文本模式的作用域一致：L2 仍然是「同一个模型条件下的簇」）
    let mut by_l1: std::collections::HashMap<String, Vec<(i64, Vec<f32>)>> =
        std::collections::HashMap::new();
    for (id, l1, _l2) in &images {
        if let Some(vec) = vec_of.get(id) {
            by_l1.entry(l1.clone()).or_default().push((*id, vec.clone()));
        }
    }

    let mut updates: Vec<(i64, String)> = Vec::new();
    for (l1, members) in by_l1.iter() {
        if let Some(filter) = only {
            if !filter.is_empty() && !filter.contains(l1) {
                continue;
            }
        }
        let labels = average_linkage(members, threshold);
        let mut buckets: std::collections::HashMap<u32, Vec<i64>> = std::collections::HashMap::new();
        for (index, (id, _)) in members.iter().enumerate() {
            buckets.entry(labels[index]).or_default().push(*id);
        }
        for (_, ids) in buckets {
            if ids.is_empty() {
                continue;
            }
            let key = vector_cluster_key(l1, &ids);
            for id in ids {
                // 手动 pin 优先：用户的合并/拆分/移动不被自动聚类带走。
                let final_key = bindings.get(&id).cloned().unwrap_or_else(|| key.clone());
                updates.push((id, final_key));
            }
        }
    }

    if updates.is_empty() {
        return Ok(0);
    }
    let conn = db.0.lock().unwrap();
    let tx = conn.unchecked_transaction()?;
    for (id, key) in &updates {
        tx.execute(
            "UPDATE images SET group_key_l2=?1 WHERE id=?2",
            rusqlite::params![key, id],
        )?;
    }
    tx.commit()?;
    Ok(updates.len())
}

/// 按当前来源的聚类方式重新分 L2。
///
/// 阈值按档位**分开**：
/// * `text_threshold` —— 词 Jaccard；
/// * `visual_threshold` —— CLIP 余弦（0.75，实测标定）；
/// * `semantic_threshold` —— BGE 余弦（**暂定值，未标定**）。
///
/// 0.75 是 CLIP 的值，**绝不可**沿用到 BGE（量纲不同）。
pub fn recluster_l2_with_mode(
    db: &Db,
    source_id: i64,
    mode: L2Mode,
    text_threshold: f64,
    visual_threshold: f64,
    semantic_threshold: f64,
) -> anyhow::Result<()> {
    match mode {
        L2Mode::Text => {
            let keys = HashSet::new();
            recluster_l2_keys(db, source_id, &keys, text_threshold)
        }
        mode => {
            let Some((slot, model_id)) = mode.signature_spec() else {
                return Ok(());
            };
            let threshold = match mode {
                L2Mode::Visual => visual_threshold,
                _ => semantic_threshold,
            };
            recluster_l2_vectors(db, source_id, slot, model_id, threshold, None)?;
            Ok(())
        }
    }
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

    /// 单链的病根：A~B、B~C 但 A≁C 时，单链会把 A、C 拉到一起。
    /// 平均连接必须把它们分开 —— 这正是阶段六换算法的全部理由。
    #[test]
    fn average_linkage_resists_chain_merging() {
        // 三维向量：A 与 B 相似，B 与 C 相似，A 与 C 不相似
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.9, 0.44, 0.0];
        let c = vec![0.0, 1.0, 0.0];
        let items = vec![(1i64, a), (2, b), (3, c)];
        let labels = average_linkage(&items, 0.6);
        assert_ne!(labels[0], labels[2], "A 与 C 不该被链到一起");

        // 单链在同样输入下会把三者拉成一组（对照）
        let single = cluster_l1(
            &vec![
                (1i64, "alpha beta".to_string()),
                (2, "beta gamma".to_string()),
                (3, "gamma delta".to_string()),
            ],
            0.33,
        );
        assert_eq!(single[0].1, single[2].1, "单链仍然会粘连（历史行为）");
    }

    /// 平均连接必须是确定性的：同一份输入跑两次得到同一份分组。
    #[test]
    fn average_linkage_is_deterministic() {
        let items: Vec<(i64, Vec<f32>)> = (0..12)
            .map(|i| {
                let angle = i as f32 * 0.7;
                (i as i64, vec![angle.cos(), angle.sin(), 0.1 * i as f32])
            })
            .collect();
        let first = average_linkage(&items, 0.5);
        let second = average_linkage(&items, 0.5);
        assert_eq!(first, second);
    }

    /// 阈值越高组越碎、越低组越紧（单调性）。
    #[test]
    fn average_linkage_respects_threshold() {
        let items: Vec<(i64, Vec<f32>)> = (0..8)
            .map(|i| (i as i64, vec![1.0, i as f32 * 0.01, 0.0]))
            .collect();
        let loose = average_linkage(&items, 0.5);
        let strict = average_linkage(&items, 0.999);
        let count = |labels: &Vec<u32>| labels.iter().collect::<std::collections::HashSet<_>>().len();
        assert!(count(&strict) >= count(&loose), "阈值更严 → 组数不应更少");
    }

    /// 组键的关键性质：**簇长大时键不能变**（否则组会「换身份」，
    /// 用户的滚动位置、保存的分组键全都会失效）。
    #[test]
    fn vector_cluster_key_is_stable_under_growth() {
        let before = vector_cluster_key("l1-a", &[7, 9, 11]);
        let after = vector_cluster_key("l1-a", &[7, 9, 11, 13]);
        assert_eq!(before, after, "成员增加不应改变组键");
        // 但不同的 L1 桶必须给出不同的键
        assert_ne!(before, vector_cluster_key("l1-b", &[7, 9, 11]));
    }

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

    /// **错空间回归守卫**（阶段七轮 2 的已登记 bug）。
    ///
    /// `run_sync` 的语义模式分支曾经用**视觉引擎**算向量，却写进
    /// `slot='text'` 的槽 —— 维度一样（都是 512 维）、条数也对，只有**值**
    /// 属于另一个空间，肉眼查不出来。
    ///
    /// 口径收口到 `L2Mode::signature_spec()` 之后，这条断言就是守门人：
    /// 语义档必须指向文本槽 + 文本模型，视觉档必须指向视觉槽 + 视觉模型。
    #[test]
    fn signature_spec_never_crosses_spaces() {
        assert_eq!(
            L2Mode::Semantic.signature_spec(),
            Some(("text", crate::text::TEXT_MODEL_ID)),
            "语义档必须读文本槽 + 文本模型"
        );
        assert_eq!(
            L2Mode::Visual.signature_spec(),
            Some(("vision", crate::vision::VISION_MODEL_ID)),
            "视觉档必须读视觉槽 + 视觉模型"
        );
        assert_eq!(L2Mode::Text.signature_spec(), None, "文本档不读向量");
        // 两个模型的 id 必须不同 —— 相同的话错空间就查不出来了。
        assert_ne!(
            crate::text::TEXT_MODEL_ID,
            crate::vision::VISION_MODEL_ID,
            "两个塔的 model_id 必须能区分（否则错空间不可察）"
        );
    }
}
