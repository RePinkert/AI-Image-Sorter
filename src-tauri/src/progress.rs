//! 分块推理的进度记账。
//!
//! ## 为什么需要这个模块
//!
//! `compute_signatures` 按 **64 张一块**调 `embed_batch`，而 `embed_batch`
//! 内部按 8 张一批上报**块内**计数。把块内计数直接当全局计数发出去，
//! 进度条就会在每块开头**重置** ——
//!
//! 实测症状：进度条在 **1% ↔ 7%** 之间反复横跳（每块覆盖全局的 64/N，
//! N=900 时正好是 7.1%），而计算本身完全正常（每批都入库了，最后结果正确）。
//!
//! 所以这里把「块内计数」换算成「全局计数」。
//!
//! 另一处口径：读图失败的张（文件被删、被占用）也要算作**已处理** ——
//! 否则有失败时进度条永远到不了 100%，用户会以为卡住了。

use std::cell::Cell;

/// 跨块累加的进度基址。
///
/// 用 `Cell` 而不是 `&mut`：`compute_signatures` 里的进度闭包要读它，
/// 外层循环又要推进它 —— `Cell` 让两者不必互相抢借用。
pub struct ProgressBase {
    base: Cell<usize>,
}

impl ProgressBase {
    pub fn new() -> Self {
        Self { base: Cell::new(0) }
    }

    /// 块内已完成张数 → 全局已完成张数。
    pub fn global(&self, done_in_chunk: usize) -> usize {
        self.base.get() + done_in_chunk
    }

    /// 一块处理完，把基址推上去。
    pub fn advance(&self, chunk_len: usize) {
        self.base.set(self.base.get() + chunk_len);
    }
}

impl Default for ProgressBase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复刻 `embed_batch` 的上报节奏：每批 8 张，最后一批可能不满。
    fn chunk_reports(len: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut d = 8;
        while d < len {
            out.push(d);
            d += 8;
        }
        out.push(len);
        out
    }

    /// 进度必须**单调不减**且**终点等于总数** —— 这正是「1%↔7% 横跳」
    /// 与「到不了 100%」两个症状各自的判据。
    #[test]
    fn progress_is_monotonic_and_reaches_total() {
        let total = 900usize;
        let base = ProgressBase::new();
        let mut seen: Vec<usize> = Vec::new();
        let mut left = total;
        while left > 0 {
            let len = left.min(64);
            for done in chunk_reports(len) {
                seen.push(base.global(done));
            }
            base.advance(len);
            left -= len;
        }
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "进度不能回退（横跳 = 回退）：{:?}",
            &seen[..12.min(seen.len())]
        );
        assert_eq!(*seen.last().unwrap(), total, "终点必须是总数");
        assert!(seen.iter().all(|v| *v <= total), "不能超过总数");
    }

    /// 跨块时不能出现「比上一块还小」的值。
    #[test]
    fn chunk_boundary_does_not_reset() {
        let base = ProgressBase::new();
        base.advance(64);
        // 第二块的第一批：块内 8 → 全局 72，而不是回到 8。
        assert_eq!(base.global(8), 72);
    }
}
