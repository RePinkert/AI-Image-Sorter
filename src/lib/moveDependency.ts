import type { SettingsSection } from '../types'

/** 后端给的实际打分基准（`MoveSuggestion.basis`）。 */
export type SuggestBasis = 'vision' | 'semantic' | 'text'

export interface DepInfo {
  /** 依赖的变量名 —— 用户要改的是它。 */
  variable: string
  /** 当前的取值（短）。 */
  value: string
  /** 悬停说明：这一栏的数字到底是什么量纲算出来的。 */
  hint: string
  /** 「去调整」按钮要跳到设置页的哪一节。 */
  target: SettingsSection
  /** 按钮文案。 */
  action: string
}

/**
 * 由「实际用的基准」+「当前档位」推出依赖声明。
 *
 * 关键分支：**档位是视觉/语义但基准退回了 text** —— 说明模型签名还没算出来，
 * 那一栏的数字此刻**不是模型给的**。这时必须说清楚，并且把用户送到
 * 「去算签名」那个设置，而不是送到「换档位」那个设置 —— 否则用户会以为
 * 换个档位就能改变结果，其实要先算模型。
 *
 * 单独成文件（而不是塞在组件里）：oxlint 的 `only-export-components` 规则
 * 不允许组件文件同时导出普通函数，而且这份判定值得被单独审。
 */
export function describeDep(basis: SuggestBasis | null, l2Mode: string): DepInfo {
  if (basis === null) {
    return {
      variable: 'L2 聚类方式',
      value: '未计算',
      hint: '还没有算出任何建议 —— 选出图片后即可计算。',
      target: 'cluster',
      action: '去调整',
    }
  }
  if (basis === 'vision') {
    return {
      variable: 'L2 聚类方式',
      value: '视觉',
      hint: '这些百分比是视觉余弦（CLIP）算出来的 —— 换档位会改变它们。',
      target: 'cluster',
      action: '去调整',
    }
  }
  if (basis === 'semantic') {
    return {
      variable: 'L2 聚类方式',
      value: '语义',
      hint: '这些百分比是语义余弦（BGE）算出来的 —— 换档位会改变它们。',
      target: 'cluster',
      action: '去调整',
    }
  }
  // basis === 'text'
  if (l2Mode !== 'text') {
    return {
      variable: '模型签名',
      value: '未就位 · 暂按文本',
      hint: `当前档位是「${l2Mode}」，但模型签名还没算出来，所以这些百分比暂时按 prompt 词重合度算。先算出签名，这一栏才会变成模型给的分数。`,
      target: 'signatures',
      action: '去计算',
    }
  }
  return {
    variable: 'L2 聚类方式',
    value: '文本',
    hint: '这些百分比是 prompt 词重合度（Jaccard）算出来的 —— 换档位会改变它们。',
    target: 'cluster',
    action: '去调整',
  }
}

/**
 * 「可能相关」一栏为空时的说明。
 *
 * 旧版是一句固定文案：「没有可推荐的目标组（需要先识图，或选中图与其它组
 * 差异过大）。」—— 它把**两种完全不同的原因**混成一句，而且常常是错的：
 * 那一栏为空多数时候是「**还在算**」，不是「没识图」。
 * 空状态说错原因 = 把用户送去错误的修复路径（军规 20：空状态要给出行动）。
 */
export function describeEmpty(basis: SuggestBasis | null, l2Mode: string): string {
  if (basis === null) {
    return '还没有可推荐的目标组。'
  }
  if (basis === 'text' && l2Mode !== 'text') {
    return '模型签名还没算出来，所以暂时按文本重合度推荐。算出签名后这一栏才会给出模型给的分数。'
  }
  return '没有找到相似度足够高的目标组。'
}
