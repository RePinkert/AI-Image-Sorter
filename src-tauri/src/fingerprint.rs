//! 内容身份：让「改了名」不等于「换了图」。
//!
//! ## 为什么需要这个模块（第 29 轮）
//!
//! 图片在库里的身份是**路径**。用户在应用之外改名 → 重扫时旧行被 DELETE、
//! 新行被 INSERT → `image_id` 变了 → 评分（`scores`）、标签（`image_labels`）、
//! 手动分组（`manual_group_bindings`）随之消失 —— 而图还是同一张图。
//!
//! 实测代价（2026-10-20 一次改名，845 个文件）：改名前的 714 条手动绑定、
//! 290 个标签、1627 条评分里，大部分被级联删除或变成孤儿行。
//!
//! ## 口径（用户 2026-10-26 拍板）
//!
//! * **主键** = (size, seed, prompt_pos, prompt_neg) —— 全部取自**内容**，
//!   与文件名无关。实测全库 0 碰撞。
//! * **主键不唯一** → 升级到**内容哈希**（见下）。
//! * **仍不唯一** → **绝不删除**，留给用户确认（见「库恢复」）。
//!
//! ## 为什么用 xxh3 而不是 sha256
//!
//! 硬约束 #1「不引入新的运行/构建依赖」—— `sha2` 不在依赖表里，而这里要
//! 的是**身份判别**、不是密码学强度：xxh3-64 在 10^6 个样本上的碰撞概率
//! 可忽略。库里已有的 `sha256` 列保持为空（它留给将来的内容级去重）。
//!
//! 标记串住在本模块 —— 实现被删 → 标记串消失 → 发行版探针立刻发现
//! （见 `ui-program.md` §产物可证）。

use std::hash::Hasher;
use std::io::Read;
use std::path::Path;

use xxhash_rust::xxh3::Xxh3;

/// 能力标记串：改名不丢分拣（第 29 轮）。
pub const CAP_RENAME_PRESERVE: &str = "ais.cap.rename.preserve.v1\0";

/// 文件内容指纹（xxh3-64，十六进制）。
///
/// **流式**计算 —— 不把整个文件读进内存（单张图实测最大 10.5 MB，但库里的
/// 文件大小不由我们决定）。
pub fn file_fingerprint(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.write(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(format!("{:016x}", hasher.finish()))
}

/// 内容主键：(size, seed, prompt_pos, prompt_neg)。
///
/// **精确串**而不是哈希 —— prompt 原文已经在内存里（解析出来的），直接用
/// 长度前缀拼起来就没有任何碰撞的可能。哈希只用在文件内容上（那里没法
/// 「精确比较」）。
pub fn content_key(size: i64, seed: i64, prompt_pos: &str, prompt_neg: &str) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        size,
        seed,
        prompt_pos.chars().count(),
        prompt_pos,
        prompt_neg.chars().count(),
        prompt_neg
    )
}

/// 元数据签名：主键撞了时的次级判定依据。
///
/// 只用**与路径无关**、且**库里已经存着**的字段 —— 否则老行和新文件就没法
/// 放在同一个天平上比。
pub fn meta_signature(
    workflow_key: &str,
    width: i64,
    height: i64,
    steps: i64,
    cfg: f64,
    recipe_signature: &str,
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        workflow_key, width, height, steps, cfg, recipe_signature
    )
}

/// 匹配依据 —— 要**如实告诉用户**「凭什么认为这两个是同一张图」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum MatchBasis {
    /// 尺寸 + 种子 + 正负 prompt 完全一致（主键唯一命中）。
    ContentKey,
    /// 除主键外的元数据完全一致（工作流指纹 / 尺寸 / 采样参数）。
    Metadata,
    /// 内容哈希一致（主键与元数据都不唯一时的最终判定）。
    Fingerprint,
}

impl MatchBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchBasis::ContentKey => "内容键（尺寸+种子+prompt）",
            MatchBasis::Metadata => "元数据（工作流+采样参数）",
            MatchBasis::Fingerprint => "内容哈希",
        }
    }
}

/// 库里的旧行（文件已不在原路径上）。
#[derive(Debug, Clone)]
pub struct RenameOld {
    pub id: i64,
    pub abs_path: String,
    pub size: i64,
    pub seed: i64,
    pub prompt_pos: String,
    pub prompt_neg: String,
    /// 除主键外的元数据签名。
    pub meta_sig: String,
    /// 内容指纹；空 = 还没算过。
    pub content_fp: String,
}

/// 盘上的新文件。
#[derive(Debug, Clone)]
pub struct RenameCandidate {
    pub size: i64,
    pub seed: i64,
    pub prompt_pos: String,
    pub prompt_neg: String,
    pub meta_sig: String,
    pub content_fp: String,
}

/// 一次改名命中。
#[derive(Debug, Clone)]
pub struct RenameMatch {
    pub old_index: usize,
    pub new_index: usize,
    pub basis: MatchBasis,
}

/// 在「旧行」与「新文件」之间做改名识别。
///
/// 三级判定，**每一级都要求两边都唯一**才命中（互斥配对）——
///
/// 1. **内容键** (size, seed, prompt_pos, prompt_neg)：主键。实测全库 0 碰撞。
/// 2. **元数据签名**（工作流指纹 + 宽高 + 采样参数）：主键撞了时的次级判定。
/// 3. **内容哈希**：前两级都不唯一时的最终判定（用户 2026-10-26 定的口径）。
///
/// 任何一级都不唯一 → **不配对**。剩下的交给用户确认，绝不静默删除。
pub fn match_renames(olds: &[RenameOld], news: &[RenameCandidate]) -> Vec<RenameMatch> {
    let mut out: Vec<RenameMatch> = Vec::new();
    let mut used_old = vec![false; olds.len()];
    let mut used_new = vec![false; news.len()];

    // 第 1 级：内容键
    {
        let key = |o: &RenameOld| content_key(o.size, o.seed, &o.prompt_pos, &o.prompt_neg);
        let key_new = |n: &RenameCandidate| {
            content_key(n.size, n.seed, &n.prompt_pos, &n.prompt_neg)
        };
        let groups = group_pairs(
            olds.len(),
            news.len(),
            |i| key(&olds[i]),
            |j| key_new(&news[j]),
        );
        for (i, j) in groups {
            used_old[i] = true;
            used_new[j] = true;
            out.push(RenameMatch {
                old_index: i,
                new_index: j,
                basis: MatchBasis::ContentKey,
            });
        }
    }

    // 第 2 级：元数据签名（只对还没配上的两边）
    {
        let groups = group_pairs(
            olds.len(),
            news.len(),
            |i| olds[i].meta_sig.clone(),
            |j| news[j].meta_sig.clone(),
        );
        for (i, j) in groups {
            if used_old[i] || used_new[j] {
                continue;
            }
            // 元数据签名必须**非空** —— 空签名等于「没有信息」，用它配对是
            // 瞎猜。
            if olds[i].meta_sig.is_empty() || news[j].meta_sig.is_empty() {
                continue;
            }
            used_old[i] = true;
            used_new[j] = true;
            out.push(RenameMatch {
                old_index: i,
                new_index: j,
                basis: MatchBasis::Metadata,
            });
        }
    }

    // 第 3 级：内容哈希
    {
        let groups = group_pairs(
            olds.len(),
            news.len(),
            |i| olds[i].content_fp.clone(),
            |j| news[j].content_fp.clone(),
        );
        for (i, j) in groups {
            if used_old[i] || used_new[j] {
                continue;
            }
            if olds[i].content_fp.is_empty() || news[j].content_fp.is_empty() {
                continue;
            }
            used_old[i] = true;
            used_new[j] = true;
            out.push(RenameMatch {
                old_index: i,
                new_index: j,
                basis: MatchBasis::Fingerprint,
            });
        }
    }

    out
}

/// 把两边按同一个键分桶，只返回「桶里旧行恰好 1 条且新文件恰好 1 个」的
/// 配对 —— 也就是**两边都唯一**才算数。
fn group_pairs(
    old_len: usize,
    new_len: usize,
    old_key: impl Fn(usize) -> String,
    new_key: impl Fn(usize) -> String,
) -> Vec<(usize, usize)> {
    use std::collections::HashMap;
    let mut by_key: HashMap<String, (Vec<usize>, Vec<usize>)> = HashMap::new();
    for i in 0..old_len {
        let k = old_key(i);
        if k.is_empty() {
            continue;
        }
        by_key.entry(k).or_default().0.push(i);
    }
    for j in 0..new_len {
        let k = new_key(j);
        if k.is_empty() {
            continue;
        }
        by_key.entry(k).or_default().1.push(j);
    }
    let mut out = Vec::new();
    for (_, (os, ns)) in by_key {
        if os.len() == 1 && ns.len() == 1 {
            out.push((os[0], ns[0]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_key_is_exact_not_hashed() {
        // 长度前缀 → 拼接歧义不可能出现：
        // ("1|23", "4") 与 ("12", "3|4") 必须得到不同的键。
        let a = content_key(1, 2, "3|4", "");
        let b = content_key(1, 2, "3", "|4");
        assert_ne!(a, b);
    }

    #[test]
    fn content_key_separates_prompt_and_negative() {
        assert_ne!(
            content_key(10, 1, "a", "b"),
            content_key(10, 1, "b", "a"),
        );
    }

    #[test]
    fn file_fingerprint_is_stable_and_content_sensitive() {
        let dir = std::env::temp_dir().join("ais-fp-test");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        std::fs::write(&a, b"hello world").unwrap();
        std::fs::write(&b, b"hello world").unwrap();
        let fa = file_fingerprint(&a).unwrap();
        let fb = file_fingerprint(&b).unwrap();
        assert_eq!(fa, fb, "同样内容必须同样指纹（改名后仍认得出）");
        std::fs::write(&b, b"hello worlD").unwrap();
        assert_ne!(fa, file_fingerprint(&b).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn old(id: i64, size: i64, seed: i64, prompt: &str, meta: &str, fp: &str) -> RenameOld {
        RenameOld {
            id,
            abs_path: format!("old/{id}.png"),
            size,
            seed,
            prompt_pos: prompt.to_string(),
            prompt_neg: String::new(),
            meta_sig: meta.to_string(),
            content_fp: fp.to_string(),
        }
    }

    fn newc(size: i64, seed: i64, prompt: &str, meta: &str, fp: &str) -> RenameCandidate {
        RenameCandidate {
            size,
            seed,
            prompt_pos: prompt.to_string(),
            prompt_neg: String::new(),
            meta_sig: meta.to_string(),
            content_fp: fp.to_string(),
        }
    }

    #[test]
    fn rename_matches_by_content_key() {
        let olds = vec![old(1, 100, 7, "cat", "wf1", "fp1")];
        let news = vec![newc(100, 7, "cat", "wf1", "fp1")];
        let m = match_renames(&olds, &news);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].basis, MatchBasis::ContentKey);
        assert_eq!(m[0].old_index, 0);
        assert_eq!(m[0].new_index, 0);
    }

    #[test]
    fn ambiguous_content_key_escalates_to_metadata() {
        // 两张图尺寸/种子/prompt 全一样（用户说的「完全一致」）→ 主键不唯一
        let olds = vec![old(1, 100, 7, "cat", "wf-A", "fpA")];
        let news = vec![newc(100, 7, "cat", "wf-A", "fpA"), newc(100, 7, "cat", "wf-B", "fpB")];
        let m = match_renames(&olds, &news);
        assert_eq!(m.len(), 1, "元数据唯一时仍要能配对");
        assert_eq!(m[0].basis, MatchBasis::Metadata);
        assert_eq!(m[0].new_index, 0);
    }

    #[test]
    fn ambiguous_everything_falls_back_to_fingerprint() {
        let olds = vec![old(1, 100, 7, "cat", "wf-A", "fpA")];
        let news = vec![newc(100, 7, "cat", "wf-A", "fpA"), newc(100, 7, "cat", "wf-A", "fpB")];
        let m = match_renames(&olds, &news);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].basis, MatchBasis::Fingerprint);
        assert_eq!(m[0].new_index, 0, "哈希不同的那个不能配对");
    }

    #[test]
    fn truly_ambiguous_is_not_matched_at_all() {
        // 两边都一模一样的两份 → 配对是瞎猜 → 不配（交给用户确认）
        let olds = vec![old(1, 100, 7, "cat", "wf-A", "fpA")];
        let news = vec![newc(100, 7, "cat", "wf-A", "fpA"), newc(100, 7, "cat", "wf-A", "fpA")];
        let m = match_renames(&olds, &news);
        assert!(m.is_empty(), "两边都唯一才配对；否则宁可不配");
    }

    #[test]
    fn one_old_two_new_same_key_is_not_matched() {
        let olds = vec![old(1, 100, 7, "cat", "wf-A", "fpA"), old(2, 100, 7, "cat", "wf-A", "fpA")];
        let news = vec![newc(100, 7, "cat", "wf-A", "fpA")];
        let m = match_renames(&olds, &news);
        assert!(m.is_empty(), "旧行有两条时不能凭主键配对");
    }
}
