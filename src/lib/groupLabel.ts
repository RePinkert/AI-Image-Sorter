import type { Granularity, GroupInfo } from '../types'

/**
 * The four classification axes of the app.
 *
 * They are PARALLEL axes, not a containment hierarchy: L0 buckets by source
 * folder, L1 by the selected model-parameter fingerprint, L2 by prompt
 * similarity, L3 by the individual prompt. An L2 cluster is not "inside" an
 * L1 — it merely shares it. Navigation between buckets is therefore LATERAL
 * (previous / next sibling on the active axis), never a drill-down.
 */
export const GRANULARITY_LEVELS: { value: Granularity; label: string }[] = [
  { value: 0, label: '文件夹' },
  { value: 1, label: 'Model偏差' },
  { value: 2, label: 'Prompt偏差' },
  { value: 3, label: '独立Prompt' },
]

export function granularityLabel(granularity: Granularity): string {
  return GRANULARITY_LEVELS.find((l) => l.value === granularity)?.label ?? '分组'
}

/** The full display name of a group on the given axis. */
export function groupLabel(g: GroupInfo, granularity: Granularity): string {
  if (granularity === 0) return g.source_path || '(文件夹)'
  if (granularity === 1) return g.model_param || g.workflow_name || '未命名 Model偏差'
  return g.prompt_pos.slice(0, 120) || '(无 prompt)'
}

/** A single-line variant for the axis strip — collapsed to one line, then
 *  truncated. Prompts carry hard newlines; a raw slice would break the row.
 *  40 字符是实测的舒适上限：再长就成了一堵墙，而不是一个可扫读的标题。 */
export function groupLabelCompact(g: GroupInfo, granularity: Granularity): string {
  const full = groupLabel(g, granularity).replace(/\s+/g, ' ').trim()
  return full.length > 40 ? `${full.slice(0, 39)}…` : full
}
