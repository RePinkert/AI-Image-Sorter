/**
 * 分组列表的视口锚点 —— 「迁移完回到上一级，别把人甩回列表顶部」。
 *
 * ## 问题
 *
 * `App.tsx` 用条件渲染挂载视图：`{view === 'groups' && <GroupList />}`。
 * 离开分组列表 → 整个组件**卸载**；而分组列表没有内部滚动容器
 * （`html/body` 没有 `overflow: hidden`，滚的是文档），所以滚动位置随组件
 * 一起消失，回来时 `scrollY = 0`。
 *
 * 对「列表下半段的零碎分组」来说，这等于每处理一个组就要重新滚回去一次。
 *
 * ## 口径
 *
 * * **锚点（anchor）**：视口顶部那一行的 `group_key` + 它距视口顶的偏移。
 *   恢复时按**行**还原，而不是按像素 —— 行被删掉时，下面整体上移，按行
 *   还原才能保证「眼睛看到的内容不变」。
 * * **定位键（focusKey）**：刚操作过的那一组。回到列表时优先落在它身上；
 *   它已经不在列表里（整组搬走了）就退回锚点。
 *
 * ## 为什么不放 zustand
 *
 * 锚点在滚动时更新（每帧量一次），进 store 会触发全店重渲染。这里用模块级
 * 缓存 —— 与既有的 `groupCache.ts` 同一模式，也**不落 localStorage**
 * （滚动位置跨进程没有意义，别占存储）。
 */

export interface ListAnchor {
  sourceId: number | null
  granularity: number
  /** 视口顶部那一行的 group_key（列表里没有行时为 null）。 */
  key: string | null
  /** 该行顶边距视口顶部的距离（负值 = 这一行已经滚过头）。 */
  offset: number
  /** 兜底：像素位置。锚点行消失时用。 */
  scrollTop: number
}

let anchor: ListAnchor | null = null
let focusKey: string | null = null

export function setListAnchor(next: ListAnchor): void {
  anchor = next
}

export function getListAnchor(): ListAnchor | null {
  return anchor
}

/** 记下「刚操作过的那一组」，下次回到列表时优先定位到它。 */
export function setFocusKey(key: string | null): void {
  focusKey = key
}

export function peekFocusKey(): string | null {
  return focusKey
}

export function clearFocusKey(): void {
  focusKey = null
}

/** 在 DOM 里找 group_key 对应的行 / 卡片。 */
export function findGroupElement(key: string): HTMLElement | null {
  const nodes = document.querySelectorAll<HTMLElement>('[data-group-key]')
  for (const node of nodes) {
    if (node.dataset.groupKey === key) return node
  }
  return null
}

/**
 * 记录当前视口顶部那一行作为锚点。
 *
 * 代价说明：`querySelectorAll` 扫一遍行，`getBoundingClientRect` 只量到
 * **第一行可见的**就停 —— 长列表滚到底部时会量较多行，但每次滚动只跑
 * 一次（调用方负责节流），实测在千行量级仍是亚毫秒级。
 */
export function captureListAnchor(sourceId: number | null, granularity: number): ListAnchor {
  const nodes = Array.from(document.querySelectorAll<HTMLElement>('[data-group-key]'))
  let key: string | null = null
  let offset = 0
  for (const node of nodes) {
    const rect = node.getBoundingClientRect()
    if (rect.bottom > 0) {
      key = node.dataset.groupKey ?? null
      offset = rect.top
      break
    }
  }
  const next: ListAnchor = {
    sourceId,
    granularity,
    key,
    offset,
    scrollTop: window.scrollY,
  }
  anchor = next
  return next
}

export interface ApplyAnchorOptions {
  sourceId: number | null
  granularity: number
  /** 用户正在滚动 → 别抢。 */
  userScrolling: boolean
}

/**
 * 恢复列表位置。
 *
 * 顺序很重要：
 *
 * 1. **先按锚点还原视口** —— 眼睛看到的内容与离开前完全一致（行被删掉
 *    导致整体上移时，这一步把它补回来）。
 * 2. **再看刚操作过的那一组**：它在不在列表里？在但不在视口内 → 滚到它；
 *    已经不在列表里了（整组被搬走）→ 什么都不做（锚点已经把人放回原处）。
 *
 * 反过来（先定位后还原）会让人被甩离刚处理过的那一行 —— 那正是要避免的。
 */
export function applyListAnchor(opts: ApplyAnchorOptions): void {
  if (opts.userScrolling) return

  // 1) 锚点还原
  const a = anchor
  if (a && a.sourceId === opts.sourceId && a.granularity === opts.granularity) {
    const el = a.key ? findGroupElement(a.key) : null
    const target = el
      ? el.getBoundingClientRect().top + window.scrollY - a.offset
      : a.scrollTop
    if (Math.abs(target - window.scrollY) > 1) window.scrollTo(0, target)
  }

  // 2) 刚操作过的那一组
  const focus = focusKey
  if (!focus) return
  const el = findGroupElement(focus)
  if (el) {
    const rect = el.getBoundingClientRect()
    const visible = rect.bottom > 0 && rect.top < window.innerHeight
    if (!visible) el.scrollIntoView({ block: 'center' })
  }
  focusKey = null
}
