import { useStore } from '../store'
import { describeDep, describeEmpty, type SuggestBasis } from '../lib/moveDependency'

export interface MoveSuggestionHeaderProps {
  /** 实际用于打分的基准。后端给的才是权威 —— 前端的 `l2Mode` 可能与库里不同步。 */
  basis: SuggestBasis | null
  /** 建议条数。 */
  count: number
  /** 推荐是否还在算。算的时候不许说「没有可推荐的目标组」。 */
  loading?: boolean
  /** 跳转前先关掉对话框（由调用方决定，因为两个调用点的关闭逻辑不同）。 */
  onNavigate?: () => void
}

/**
 * 「模型建议」栏的页眉 + 依赖声明 + 联动设置按钮（第 28 轮 · 跨页 UX 关联）。
 *
 * ## 为什么必须有「依赖：{变量}」
 *
 * 这一栏的百分比**不是客观事实** —— 它由某个设置决定（`L2 聚类方式` 与它
 * 对应的模型签名）。用户看到「87%」时，无从知道那是 CLIP 余弦还是词重合度，
 * 而**这两个量纲不可通约**（见 `types.ts` 的 `MoveSuggestion.basis` 注释）。
 *
 * 更糟的是：栏内每条建议的分数徽章原本**只靠颜色**区分基准（`--accent`
 * vs `--muted`）。栏头一旦把基准写成**文字**，颜色就退化成冗余强化 ——
 * 这正是军规 15「不只靠颜色传递信息」在本产品里的落点。
 *
 * ## 联动
 *
 * 「去调整」直接跳到设置页的对应分区（`settingsFocus`）—— 用户不必自己
 * 去找那个旋钮。设置页按分区组织正是为了让这个跳转有落点。
 */
export function MoveSuggestionHeader({ basis, count, loading, onNavigate }: MoveSuggestionHeaderProps) {
  const l2Mode = useStore((s) => s.l2Mode)
  const setView = useStore((s) => s.setView)
  const setSettingsFocus = useStore((s) => s.setSettingsFocus)

  const dep = describeDep(basis, l2Mode)

  function goToSettings() {
    onNavigate?.()
    setSettingsFocus(dep.target)
    setView('settings')
  }

  return (
    <>
      <h4 className="move-column-title">
        可能相关 <span className="muted">{count}</span>
      </h4>
      <p className="move-dep" title={dep.hint}>
        <span className="move-dep-label">依赖：</span>
        <span className="move-dep-var">{dep.variable}</span>
        <span className="move-dep-value">{loading ? '计算中' : dep.value}</span>
        <button type="button" className="btn-tertiary move-dep-go" onClick={goToSettings}>
          {dep.action}
        </button>
      </p>
    </>
  )
}

/**
 * 「可能相关」一栏在**没有内容**时该显示什么。
 *
 * 分三种状态，缺一不可：
 * - 还在算 → 「正在计算…」（`role="status"`）—— 绝不能在这时候说「没有可推荐」
 * - 算完了但为空 → 按**真实原因**给一句话（见 `describeEmpty`）
 * - 有内容 → 不渲染本组件
 */
export function MoveSuggestionEmpty({ basis, loading }: { basis: SuggestBasis | null; loading?: boolean }) {
  const l2Mode = useStore((s) => s.l2Mode)
  if (loading) {
    return (
      <p className="muted" role="status">
        正在计算…
      </p>
    )
  }
  return <p className="muted">{describeEmpty(basis, l2Mode)}</p>
}
