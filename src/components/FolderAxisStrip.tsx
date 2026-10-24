// The "where am I in this classification" strip.
//
// The app's four levels (文件夹 / Model偏差 / Prompt偏差 / 独立Prompt) are
// PARALLEL classification axes, not a containment hierarchy: an L2 prompt
// cluster is not "inside" an L1 model group, it merely shares it. So the
// navigation between buckets is lateral — previous / next sibling on the
// active axis — and this strip states the axis and the position instead of
// drawing a fake path.
interface Props {
  /** The active axis, e.g. "Prompt偏差". */
  axisLabel: string
  /** Index of the current bucket on that axis, or -1 when unknown. */
  index: number
  /** Total number of buckets on that axis. */
  total: number
  /** Display name of the current bucket (single line, truncated). */
  label: string
  /** The full name, shown on hover. */
  title: string
  onPrev: () => void
  onNext: () => void
}

export function FolderAxisStrip({ axisLabel, index, total, label, title, onPrev, onNext }: Props) {
  const canPrev = index > 0
  const canNext = index >= 0 && index < total - 1
  return (
    <div className="fb-axis">
      <span className="fb-axis-ident">
        <span className="fb-axis-name">{axisLabel}</span>
        <span className="fb-axis-pos">
          {index >= 0 ? `第 ${index + 1} / ${total} 组` : `共 ${total} 组`}
        </span>
      </span>
      <span className="fb-axis-label" title={title}>{label}</span>
      <span className="fb-axis-nav">
        <button
          type="button"
          className="btn-tertiary"
          disabled={!canPrev}
          onClick={onPrev}
          title="上一组（←）"
        >
          ‹ 上一组
        </button>
        <button
          type="button"
          className="btn-tertiary"
          disabled={!canNext}
          onClick={onNext}
          title="下一组（→）"
        >
          下一组 ›
        </button>
      </span>
    </div>
  )
}
