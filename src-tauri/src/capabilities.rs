//! 构建能力自述（发行版 EXE 的可证机制）
//!
//! ## 为什么需要这个模块
//!
//! 发行版 EXE 是**用户主动审查的产物** —— 用户要能回答「这个 exe 里到底
//! 有没有那套新能力」。而光看文件大小 / 时间戳答不了这个问题：
//!
//! 阶段七轮 2 的教训 —— 整座文本塔是**死代码**（`text.rs` 挂着
//! `#![allow(dead_code)]`，`TextEngine::embed_prompts` 无调用方），再叠加
//! `Cargo.toml` 的 `opt-level="s" + lto=true + strip=true`，链接器把整座塔
//! 当垃圾剔除了。结果：轮 2 的 exe 与轮 1 **逐字节等价**，功能上什么都没变。
//! 交付那一刻没人看出来，因为「没有变化」这件事本身不可见。
//!
//! ## 这个模块的纪律
//!
//! **每个能力标记串都住在实现它的那个模块里**，本模块只负责收集。
//!
//! * 删掉实现 → 常量消失 → 标记串从二进制里消失 → 探针立刻发现；
//! * 加一个只在文档里声称、代码里没有的能力 → 标记串不会出现在二进制里
//!   → 探针立刻发现。
//!
//! 所以标记串不是贴上去的标签，是实现的**副产品**，不可能变成谎言。
//!
//! 探针（`scripts/verify-release-binary.ps1`）按 [`MARKER_PREFIX`] 在二进制
//! 里扫标记串，并逐个核对实现路径上的特征串（两者都在才算能力就位）。
//!
//! ## 作用层级
//!
//! 本轮全部能力都作用在 **L2（Prompt偏差）** 这一根轴上。其余轴
//! （L0 文件夹 / L1 Model偏差 / L3 独立Prompt）没有模糊性可言，本轮不动。

use serde::Serialize;

/// 能力标记的公共前缀。发行版探针按这个前缀在二进制里扫标记串。
pub const MARKER_PREFIX: &str = "ais.cap.";

/// 取出标记串的可见部分。
///
/// 标记串在二进制里**以 NUL 结尾**（C 风格哨兵）。原因：Rust 的字符串
/// 字面量不带终止符，而 `.rodata` 里相邻的字面量会紧挨着 —— 探针若按
/// 「读到不可打印字节为止」提取，就会把下一条字面量也吞进来
/// （实测踩过：`ais.cap.l2.text-jaccard.v1` 后面紧跟着 `L2`，被读成了
/// `...v1L2`）。NUL 哨兵让提取没有歧义。
pub fn marker_id(marker: &str) -> &str {
    marker.trim_end_matches('\0')
}

/// 本模块自己的标记（构建能力自述）。
pub const CAP_PROVENANCE: &str = "ais.cap.provenance.build-info.v1\0";

/// 能力类别：聚类手段。
pub const KIND_CLUSTERING: &str = "clustering";
/// 能力类别：模型塔（视觉 / 文本）。
pub const KIND_TOWER: &str = "tower";
/// 能力类别：元能力（本模块）。
pub const KIND_PROVENANCE: &str = "provenance";

/// 一条能力条目。
#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    /// 稳定 ID（前端 key、文档引用）。
    pub id: &'static str,
    /// 二进制里能被探针扫到的标记串 —— **住在实现模块里**。
    pub marker: &'static str,
    /// UI 显示名。
    pub label: &'static str,
    /// 类别（clustering / tower / provenance）。
    pub kind: &'static str,
    /// 作用层级。本轮只有 L2。
    pub axis: &'static str,
}

/// 本次构建实际具备的能力（**运行时真值**）。
///
/// 这里引用的每一个常量都住在**实现该能力**的模块里：
///
/// * `clustering::CAP_L2_*` → `clustering.rs` 的聚类实现；
/// * `vision::CAP_TOWER_VISION` → `vision.rs` 的视觉塔。
///
/// 文本塔的条目**故意不在这里**：在它接线之前（阶段七轮 3）`text.rs` 是
/// 死代码，引用它的任何常量都会把标记串硬拽进二进制 —— 那就等于给探针
/// 撒谎。接线那一档再把条目加进来。
pub fn capabilities() -> Vec<Capability> {
    let mut out = Vec::new();
    out.push(Capability {
        id: "l2.text-jaccard",
        marker: marker_id(crate::clustering::CAP_L2_TEXT_JACCARD),
        label: "L2 词 Jaccard 单链",
        kind: KIND_CLUSTERING,
        axis: "L2",
    });
    out.push(Capability {
        id: "l2.visual-avg-linkage",
        marker: marker_id(crate::clustering::CAP_L2_VISUAL),
        label: "L2 视觉塔平均连接",
        kind: KIND_CLUSTERING,
        axis: "L2",
    });
    out.push(Capability {
        id: "tower.vision",
        marker: marker_id(crate::vision::CAP_TOWER_VISION),
        label: "视觉塔（CLIP ViT-B/32）",
        kind: KIND_TOWER,
        axis: "L2",
    });
    // 文本塔：从阶段七轮 1 起**真的接进命令层**了（`compute_text_signatures`），
    // 所以它的标记串现在会出现在二进制里 —— 轮 2 之前它不在（整座塔被 LTO
    // 剔除了）。这条标记串就是那次教训的守门人：它一消失就说明文本塔又被
    // 变成死代码了。
    out.push(Capability {
        id: "tower.text",
        marker: marker_id(crate::text::CAP_TOWER_TEXT),
        label: "文本塔（BGE-small-zh）",
        kind: KIND_TOWER,
        axis: "L2",
    });
    out.push(Capability {
        id: "provenance.build-info",
        marker: marker_id(CAP_PROVENANCE),
        label: "构建能力自述",
        kind: KIND_PROVENANCE,
        axis: "-",
    });
    // 每条标记都必须带公共前缀 —— 探针按前缀扫二进制，前缀错了就扫不到。
    debug_assert!(
        out.iter().all(|c| c.marker.starts_with(MARKER_PREFIX)),
        "能力标记必须以 {MARKER_PREFIX} 开头，否则发行版探针扫不到：{:?}",
        out.iter().map(|c| c.marker).collect::<Vec<_>>()
    );
    out
}
