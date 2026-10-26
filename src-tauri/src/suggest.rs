//! 「可能相关」推荐（阶段六 Round 3）
//!
//! 用户在最痛的地方是：在大组里发现两三张图被错误识别，想把它们挪走，
//! 但目标组在一长串里根本看不出来。这个模块回答那个问题：
//! **给定选中的图，哪个组最可能该收留它们。**
//!
//! 打分口径（两种基准，按可用性自动选择）：
//!
//! | 基准 | 口径 | 何时用 |
//! |---|---|---|
//! | 视觉 | 组分数 = 选中各图的「与组内成员的最大余弦」的平均 | 该图有视觉签名时 |
//! | 文本 | 同上，但用 prompt 的词 Jaccard | 没有签名时的退路（永远可用） |
//!
//! 用「最大」而不是「平均」：一个组里只要有**一张**跟你的图很像，它就是
//! 候选；用平均会被大组稀释。

use std::collections::{HashMap, HashSet};

/// 一条推荐。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub group_key: String,
    pub count: i64,
    pub prompt_pos: String,
    /// 0..1 的相似度（两种基准的量纲不同，UI 要分开显示）。
    pub score: f64,
    /// 'vision'（视觉相似）或 'text'（文本相似）。
    pub basis: &'static str,
}

/// 组 → 成员。
pub struct GroupMembers {
    pub group_key: String,
    pub count: i64,
    pub prompt_pos: String,
    pub members: Vec<i64>,
}

/// 词集合（文本基准用）。
pub type TokenSets = HashMap<i64, HashSet<String>>;

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut inter = 0usize;
    for t in a {
        if b.contains(t) {
            inter += 1;
        }
    }
    let union = a.len() + b.len() - inter;
    if union == 0 {
        return 1.0;
    }
    inter as f64 / union as f64
}

/// 给候选组打分并按分数降序返回。
///
/// `vectors` 是该槽位下的签名（没有就退回文本基准）。
/// `tokens` 是文本基准用的词集合（每个图一条）。
/// `vector_basis` 是**有向量时**该标成什么基准 —— 由调用方按槽位给
/// （视觉槽 → `vision`，文本槽 → `semantic`）。两者的余弦量纲不同，
/// 标错会让用户以为「24% 的视觉相似」其实是语义相似。
pub fn score_groups(
    groups: &[GroupMembers],
    selected: &[i64],
    vectors: &HashMap<i64, Vec<f32>>,
    tokens: &TokenSets,
    vector_basis: &'static str,
) -> Vec<Suggestion> {
    let selected: Vec<i64> = selected.to_vec();
    if selected.is_empty() || groups.is_empty() {
        return Vec::new();
    }
    // 有多少张选中的图有视觉签名 —— 决定用哪个基准
    let with_vector = selected.iter().filter(|id| vectors.contains_key(id)).count();
    let use_vision = with_vector > 0;

    let mut out: Vec<Suggestion> = Vec::with_capacity(groups.len());
    for group in groups {
        // 组内成员的可比集合：视觉基准只用有向量的成员
        let member_vectors: Vec<&Vec<f32>> = if use_vision {
            group
                .members
                .iter()
                .filter_map(|id| vectors.get(id))
                .collect()
        } else {
            Vec::new()
        };
        let member_tokens: Vec<&HashSet<String>> = if use_vision {
            Vec::new()
        } else {
            group
                .members
                .iter()
                .filter_map(|id| tokens.get(id))
                .collect()
        };
        if use_vision && member_vectors.is_empty() {
            continue;
        }
        if !use_vision && member_tokens.is_empty() {
            continue;
        }

        let mut total = 0f64;
        let mut counted = 0usize;
        for id in &selected {
            let best = if use_vision {
                let Some(vec) = vectors.get(id) else { continue };
                member_vectors
                    .iter()
                    .map(|m| cosine_of(vec, m))
                    .fold(f32::NEG_INFINITY, f32::max)
            } else {
                let Some(tok) = tokens.get(id) else { continue };
                member_tokens
                    .iter()
                    .map(|m| jaccard(tok, m) as f32)
                    .fold(f32::NEG_INFINITY, f32::max)
            };
            if best.is_finite() {
                total += best as f64;
                counted += 1;
            }
        }
        if counted == 0 {
            continue;
        }
        out.push(Suggestion {
            group_key: group.group_key.clone(),
            count: group.count,
            prompt_pos: group.prompt_pos.clone(),
            score: total / counted as f64,
            basis: if use_vision { vector_basis } else { "text" },
        });
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.count.cmp(&a.count))
    });
    out
}

fn cosine_of(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut sum = 0f32;
    for i in 0..n {
        sum += a[i] * b[i];
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(key: &str, members: Vec<i64>) -> GroupMembers {
        GroupMembers {
            group_key: key.to_string(),
            count: members.len() as i64,
            prompt_pos: "p".to_string(),
            members,
        }
    }

    /// 选中的图与某个组里的某张图很像 → 那个组应当排第一。
    #[test]
    fn ranks_the_visually_closest_group_first() {
        let mut vectors: HashMap<i64, Vec<f32>> = HashMap::new();
        vectors.insert(1, vec![1.0, 0.0]);
        vectors.insert(2, vec![0.99, 0.1]);
        vectors.insert(3, vec![0.0, 1.0]);
        vectors.insert(4, vec![0.0, 0.99]);
        let groups = vec![group("far", vec![3, 4]), group("near", vec![1, 2])];
        let out = score_groups(&groups, &[1], &vectors, &HashMap::new(), "vision");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].group_key, "near");
        assert_eq!(out[0].basis, "vision");
        assert!(out[0].score > out[1].score);
    }

    /// 没有视觉签名时退回文本基准，且仍然可用。
    #[test]
    fn falls_back_to_text_similarity() {
        let mut tokens: TokenSets = HashMap::new();
        tokens.insert(1, ["alpha", "beta"].iter().map(|s| s.to_string()).collect());
        tokens.insert(2, ["alpha", "beta"].iter().map(|s| s.to_string()).collect());
        tokens.insert(3, ["gamma"].iter().map(|s| s.to_string()).collect());
        let groups = vec![group("other", vec![3]), group("same", vec![2])];
        let out = score_groups(&groups, &[1], &HashMap::new(), &tokens, "vision");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].group_key, "same");
        assert_eq!(out[0].basis, "text");
    }

    /// 选中多张图时取平均：只跟其中一张像的组，分数应当被拉低。
    #[test]
    fn averages_over_selected_images() {
        let mut vectors: HashMap<i64, Vec<f32>> = HashMap::new();
        vectors.insert(1, vec![1.0, 0.0]);
        vectors.insert(2, vec![0.0, 1.0]);
        vectors.insert(3, vec![1.0, 0.0]);
        vectors.insert(4, vec![0.0, 1.0]);
        let groups = vec![group("a", vec![3]), group("b", vec![4])];
        let out = score_groups(&groups, &[1, 2], &vectors, &HashMap::new(), "vision");
        // 两组各只跟一半像 → 分数应当接近且都不高
        assert_eq!(out.len(), 2);
        assert!(out[0].score < 0.9, "只跟一半像的组不该拿高分：{}", out[0].score);
    }
}
