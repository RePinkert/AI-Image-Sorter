import { useEffect, useState, type RefObject } from 'react'

export interface ItemPos {
  top: number
  h: number
}

export interface ViewportWindow {
  start: number
  end: number
}

const FULL_WINDOW: ViewportWindow = { start: 0, end: Number.MAX_SAFE_INTEGER }
const EMPTY_WINDOW: ViewportWindow = { start: 0, end: 0 }

/**
 * Tracks the index range of items whose vertical spans intersect the
 * container's scroll viewport (plus `overscan` px on both sides). Positions
 * are absolute layout coordinates of absolute-positioned children inside the
 * scroll container; they are not assumed to be strictly sorted by top, so the
 * scan is O(n) over plain numbers per scroll frame (no state churn from the
 * layout itself — it never re-renders unless the window edges move).
 */
export function useViewportWindow(
  containerRef: RefObject<HTMLElement | null>,
  positions: ItemPos[] | null,
  overscan = 1200,
): ViewportWindow {
  const [range, setRange] = useState<ViewportWindow>(EMPTY_WINDOW)

  useEffect(() => {
    const el = containerRef.current
    if (!el) return undefined

    let raf = 0
    let lastStart = -1
    let lastEnd = -1

    const update = () => {
      raf = 0
      if (!positions) return
      const top = el.scrollTop
      const bottom = top + el.clientHeight
      let start = positions.length
      let end = -1
      for (let i = 0; i < positions.length; i++) {
        const p = positions[i]
        if (p.top + p.h < top - overscan || p.top > bottom + overscan) continue
        if (i < start) start = i
        if (i > end) end = i
      }
      if (end === -1) {
        if (lastStart !== -1) {
          lastStart = 0
          lastEnd = -1
          setRange(EMPTY_WINDOW)
        }
        return
      }
      if (start !== lastStart || end !== lastEnd) {
        lastStart = start
        lastEnd = end
        setRange(start > end ? FULL_WINDOW : { start, end })
      }
    }

    const schedule = () => {
      if (raf === 0) raf = requestAnimationFrame(update)
    }

    update()
    el.addEventListener('scroll', schedule, { passive: true })
    const ro = new ResizeObserver(schedule)
    ro.observe(el)
    return () => {
      el.removeEventListener('scroll', schedule)
      ro.disconnect()
      if (raf !== 0) cancelAnimationFrame(raf)
    }
  }, [containerRef, positions, overscan])

  return positions ? range : FULL_WINDOW
}
