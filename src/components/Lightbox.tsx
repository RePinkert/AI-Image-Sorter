import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'framer-motion'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useStore } from '../store'
import { matchesBinding } from '../keymap'
import { isWebDev } from '../api'
import type { ImageRow } from '../types'
import { ImageMetaPopover } from './ImageMetaPopover'
import { FilmStrip } from './FilmStrip'

// A real image viewer, ported from the interaction model of a mature local
// photo manager (Lap): the zoom is a real transform model rather than a
// CSS `max-width` trick.
//
// Model (same shape as Lap's Image.vue):
//   - The image is laid out at its NATURAL pixel size and then transformed.
//   - `scale` is an absolute multiplier of the natural size (1 = natural CSS
//     pixels; 1/dpr = "actual size" on a HiDPI screen).
//   - `pos` is the CENTER of the image in stage coordinates, so the zoom
//     anchor math stays simple and the image can never be dragged away.
//
// Capabilities: fit / 1:1, cursor-anchored wheel zoom (fixed 20% per notch),
// trackpad pinch, clamped panning, horizontal drag = next/previous when the
// image fits, ←/→ paging, native (Tauri) fullscreen, status readout and a
// filmstrip of the surrounding images.
//
// Layout: the metadata panel lives in a right rail and NEVER overlays the
// zoomed image — opening an image for a full look always yields an
// unobstructed image.
interface Props {
  src: string
  onClose: () => void
  /** Optional: when provided, render a right sidebar with a collapsible
   *  metadata panel (prompt / model / seed…) so a zoomed image can also be
   *  inspected — used by FolderView where there is no other way to see an
   *  image's metadata. */
  meta?: ImageRow
  onMetaUpdated?: (image: ImageRow) => void
  /** The images of the same folder, in display order. Enables ←/→ paging
   *  and the filmstrip. */
  siblings?: ImageRow[]
  /** Index of the current image inside `siblings`. */
  index?: number
  /** Called when the user pages to another image. The owner swaps the
   *  current image; the viewer resets itself to "fit" afterwards. */
  onNavigate?: (index: number) => void
}

const MIN_SCALE = 0.1
const MAX_SCALE = 10
const WHEEL_STEP = 0.2
const DRAG_NAV_THRESHOLD = 80

type ZoomMode = 'fit' | 'actual' | 'free'

interface View {
  scale: number
  x: number
  y: number
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, v))
}

export function Lightbox({ src, onClose, meta, onMetaUpdated, siblings, index, onNavigate }: Props) {
  const [view, setView] = useState<View>({ scale: 1, x: 0, y: 0 })
  const [mode, setMode] = useState<ZoomMode>('fit')
  const [stageSize, setStageSize] = useState({ w: 0, h: 0 })
  const [natural, setNatural] = useState({ w: 0, h: 0 })
  const [metaOpen, setMetaOpen] = useState(false)
  const [fullscreen, setFullscreen] = useState(false)
  const [dragging, setDragging] = useState(false)
  const [hintVisible, setHintVisible] = useState(true)

  const stageRef = useRef<HTMLDivElement | null>(null)
  const imgRef = useRef<HTMLImageElement | null>(null)
  const wrapRef = useRef<HTMLDivElement | null>(null)
  const animTimer = useRef<number | null>(null)
  // Mirrors of the live values so fast wheel/pointer events never read a
  // stale closure.
  const viewRef = useRef<View>(view)
  const stageRefValue = useRef(stageSize)
  const naturalRef = useRef(natural)
  const pointerRef = useRef<{
    x: number
    y: number
    moved: boolean
    downX: number
    downY: number
    navX: number
    pointerId: number
  } | null>(null)
  // Set when a drag starts: the browser fires a synthetic `click` after the
  // pointer is released, and it must not be mistaken for a click.
  const suppressClickRef = useRef(false)
  const wasMaximizedRef = useRef(false)

  const keybindings = useStore((s) => s.keybindings)
  const closeBinding = keybindings.closeLightbox
  const prevBinding = keybindings.viewerPrev
  const nextBinding = keybindings.viewerNext
  const zoomInBinding = keybindings.viewerZoomIn
  const zoomOutBinding = keybindings.viewerZoomOut
  const fitBinding = keybindings.viewerFit
  const actualBinding = keybindings.viewerActual
  const fullscreenBinding = keybindings.viewerFullscreen
  const metaBinding = keybindings.viewerMeta

  const applyView = useCallback((next: View) => {
    viewRef.current = next
    setView(next)
  }, [])

  // Direct-wheel and drag panning must feel instant: the CSS transition on
  // the transform is suspended while the pointer is in motion and restored
  // a moment after it stops. Mutating the style directly avoids a re-render
  // per wheel event.
  const suppressAnim = useCallback(() => {
    const el = wrapRef.current
    if (!el) return
    el.style.transition = 'none'
    if (animTimer.current) window.clearTimeout(animTimer.current)
    animTimer.current = window.setTimeout(() => {
      if (wrapRef.current) wrapRef.current.style.transition = ''
    }, 140)
  }, [])

  // Natural size: the DB already knows it, so the viewer can be laid out
  // before the file finishes decoding. The <img> load event corrects it if
  // the stored dimensions were wrong.
  useEffect(() => {
    const w = meta?.width ?? 0
    const h = meta?.height ?? 0
    const next = { w: w > 0 ? w : 0, h: h > 0 ? h : 0 }
    naturalRef.current = next
    setNatural(next)
  }, [meta?.id, meta?.width, meta?.height])

  const fitScale = useMemo(() => {
    if (!natural.w || !natural.h || !stageSize.w || !stageSize.h) return 1
    return Math.min(stageSize.w / natural.w, stageSize.h / natural.h)
  }, [natural, stageSize])

  const actualScale = useMemo(() => 1 / (typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1), [])

  // Keep the image inside the stage: when it is smaller than the stage it is
  // re-centred, when it is bigger it may be panned but never dragged away.
  const clampPos = useCallback((p: View, scale: number): View => {
    const stage = stageRefValue.current
    const nat = naturalRef.current
    if (!stage.w || !stage.h || !nat.w || !nat.h) return p
    const hx = (nat.w * scale) / 2
    const hy = (nat.h * scale) / 2
    return {
      scale,
      x: hx * 2 > stage.w ? clamp(p.x, stage.w - hx, hx) : stage.w / 2,
      y: hy * 2 > stage.h ? clamp(p.y, stage.h - hy, hy) : stage.h / 2,
    }
  }, [])

  // Re-fit whenever the stage or the image changes size.
  useEffect(() => {
    const el = stageRef.current
    if (!el) return
    const measure = () => {
      const rect = el.getBoundingClientRect()
      const next = { w: rect.width, h: rect.height }
      stageRefValue.current = next
      setStageSize(next)
    }
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  // Apply the zoom mode whenever it (or the geometry it depends on) changes.
  useEffect(() => {
    if (!stageSize.w || !stageSize.h || !natural.w || !natural.h) return
    if (mode === 'free') {
      const cur = viewRef.current
      applyView(clampPos(cur, cur.scale))
      return
    }
    const scale = mode === 'fit' ? fitScale : actualScale
    applyView(clampPos({ scale, x: stageSize.w / 2, y: stageSize.h / 2 }, scale))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, fitScale, actualScale, stageSize.w, stageSize.h, natural.w, natural.h])

  // A new image always starts from "fit".
  //
  // The fit transform is applied HERE rather than left to the mode effect
  // above: that effect runs once with the *previous* mode ('actual') as soon
  // as the new image's natural size lands, which flashed the new picture at
  // 1:1 for a frame before snapping to fit.
  useEffect(() => {
    setMode('fit')
    const stage = stageRefValue.current
    const nat = naturalRef.current
    if (stage.w && stage.h && nat.w && nat.h) {
      const scale = Math.min(stage.w / nat.w, stage.h / nat.h)
      applyView({ scale, x: stage.w / 2, y: stage.h / 2 })
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [src])

  const zoomBy = useCallback((factor: number, anchor?: { x: number; y: number }) => {
    const cur = viewRef.current
    const next = clamp(cur.scale * factor, MIN_SCALE, MAX_SCALE)
    if (next === cur.scale) return
    const ratio = next / cur.scale
    const nextPos = anchor
      ? {
          x: cur.x + (anchor.x - cur.x) * (1 - ratio),
          y: cur.y + (anchor.y - cur.y) * (1 - ratio),
        }
      : { x: cur.x, y: cur.y }
    applyView(clampPos({ scale: next, ...nextPos }, next))
    setMode('free')
  }, [applyView, clampPos])

  // Wheel zoom must be a NON-passive listener: React attaches onWheel
  // passively, so preventDefault() there would not stop the page from
  // scrolling. Fixed 20% per notch, anchored at the cursor; a trackpad pinch
  // (ctrl + small wheel deltas) uses the exponential Chromium-style curve.
  useEffect(() => {
    const el = stageRef.current
    if (!el) return
    const onWheel = (e: WheelEvent) => {
      e.preventDefault()
      suppressAnim()
      const rect = el.getBoundingClientRect()
      const cursor = { x: e.clientX - rect.left, y: e.clientY - rect.top }
      const cur = viewRef.current
      const isPinch = e.ctrlKey && e.deltaMode === 0 && Math.abs(e.deltaY) < 50
      let factor: number
      if (isPinch) {
        factor = Math.exp(-e.deltaY / 96)
      } else {
        let delta = e.deltaY
        if (e.deltaMode === 1) delta *= 40
        else if (e.deltaMode === 2) delta *= 800
        const notches = Math.sign(delta) * (Math.abs(delta) / 100)
        if (notches === 0) return
        factor = Math.pow(1 + WHEEL_STEP, -notches)
      }
      const next = clamp(cur.scale * factor, MIN_SCALE, MAX_SCALE)
      const ratio = next / cur.scale
      applyView(clampPos({
        scale: next,
        x: cur.x + (cursor.x - cur.x) * (1 - ratio),
        y: cur.y + (cursor.y - cur.y) * (1 - ratio),
      }, next))
      setMode('free')
    }
    el.addEventListener('wheel', onWheel, { passive: false })
    return () => el.removeEventListener('wheel', onWheel)
  }, [applyView, clampPos, suppressAnim])

  const canPan = useMemo(() => {
    if (!natural.w || !natural.h || !stageSize.w || !stageSize.h) return false
    return natural.w * view.scale > stageSize.w || natural.h * view.scale > stageSize.h
  }, [natural, stageSize, view.scale])

  // Pointer drag: pan when the image is bigger than the stage, otherwise a
  // horizontal drag pages to the neighbouring image.
  //
  // Pointer capture is taken **lazily** — only once the pointer has moved
  // past the drag threshold. Capturing on pointerdown would retarget the
  // subsequent `click` event to the stage, and every button inside the stage
  // (HUD, prev/next) would lose its own click handler while the stage's
  // "backdrop click closes" branch fired instead.
  useEffect(() => {
    const el = stageRef.current
    if (!el) return
    const onPointerDown = (e: PointerEvent) => {
      if (e.button !== 0) return
      pointerRef.current = {
        x: e.clientX,
        y: e.clientY,
        moved: false,
        downX: e.clientX,
        downY: e.clientY,
        navX: 0,
        pointerId: e.pointerId,
      }
      suppressClickRef.current = false
    }
    const onPointerMove = (e: PointerEvent) => {
      const p = pointerRef.current
      if (!p) return
      const dx = e.clientX - p.x
      const dy = e.clientY - p.y
      if (!p.moved) {
        if (Math.hypot(e.clientX - p.downX, e.clientY - p.downY) <= 4) return
        p.moved = true
        setDragging(true)
        // This is a drag, not a click: capture now so the pan keeps working
        // outside the stage, and suppress the synthetic click that follows
        // the release.
        suppressClickRef.current = true
        try {
          el.setPointerCapture(p.pointerId)
        } catch {
          // Capture is best-effort; the drag still works without it.
        }
      }
      if (canPan) {
        suppressAnim()
        const cur = viewRef.current
        applyView(clampPos({ scale: cur.scale, x: cur.x + dx, y: cur.y + dy }, cur.scale))
      } else {
        p.navX += dx
      }
      p.x = e.clientX
      p.y = e.clientY
    }
    const onPointerUp = () => {
      const p = pointerRef.current
      pointerRef.current = null
      if (!p) return
      setDragging(false)
      if (!p.moved) return
      if (!canPan && Math.abs(p.navX) > DRAG_NAV_THRESHOLD && siblings && onNavigate && index != null) {
        const delta = p.navX < 0 ? 1 : -1
        const next = index + delta
        if (next >= 0 && next < siblings.length) onNavigate(next)
      }
    }
    el.addEventListener('pointerdown', onPointerDown)
    el.addEventListener('pointermove', onPointerMove)
    el.addEventListener('pointerup', onPointerUp)
    el.addEventListener('pointercancel', onPointerUp)
    return () => {
      el.removeEventListener('pointerdown', onPointerDown)
      el.removeEventListener('pointermove', onPointerMove)
      el.removeEventListener('pointerup', onPointerUp)
      el.removeEventListener('pointercancel', onPointerUp)
    }
  }, [applyView, canPan, clampPos, siblings, onNavigate, index, suppressAnim])

  // Native fullscreen through the Tauri window (never the browser
  // Fullscreen API: on Windows the window manager owns the maximized state
  // and only the native call restores it correctly).
  const toggleFullscreen = useCallback(async () => {
    if (isWebDev()) return
    try {
      const appWindow = getCurrentWindow()
      if (fullscreen) {
        await appWindow.setFullscreen(false)
        if (wasMaximizedRef.current) await appWindow.maximize()
        wasMaximizedRef.current = false
        setFullscreen(false)
        return
      }
      wasMaximizedRef.current = await appWindow.isMaximized()
      if (wasMaximizedRef.current) await appWindow.unmaximize()
      await appWindow.setFullscreen(true)
      setFullscreen(true)
    } catch {
      // A window that refuses fullscreen must not break the viewer.
      setFullscreen(false)
    }
  }, [fullscreen])

  // If the user leaves fullscreen through the window manager, follow it.
  useEffect(() => {
    if (isWebDev()) return
    let unlisten: (() => void) | undefined
    let disposed = false
    void getCurrentWindow().onResized(async () => {
      if (disposed || !fullscreen) return
      try {
        const isFs = await getCurrentWindow().isFullscreen()
        if (!isFs) {
          if (wasMaximizedRef.current) await getCurrentWindow().maximize()
          wasMaximizedRef.current = false
          setFullscreen(false)
        }
      } catch { /* ignore */ }
    }).then((un) => {
      if (disposed) un()
      else unlisten = un
    })
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [fullscreen])

  // Keyboard: every capability of the viewer is reachable without a mouse.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (matchesBinding(closeBinding, e)) {
        e.preventDefault()
        onClose()
        return
      }
      if (siblings && onNavigate && index != null) {
        if (matchesBinding(prevBinding, e)) {
          e.preventDefault()
          if (index > 0) onNavigate(index - 1)
          return
        }
        if (matchesBinding(nextBinding, e)) {
          e.preventDefault()
          if (index < siblings.length - 1) onNavigate(index + 1)
          return
        }
      }
      if (matchesBinding(zoomInBinding, e)) {
        e.preventDefault()
        zoomBy(1.25)
        return
      }
      if (matchesBinding(zoomOutBinding, e)) {
        e.preventDefault()
        zoomBy(1 / 1.25)
        return
      }
      if (matchesBinding(fitBinding, e)) {
        e.preventDefault()
        setMode('fit')
        return
      }
      if (matchesBinding(actualBinding, e)) {
        e.preventDefault()
        setMode('actual')
        return
      }
      if (matchesBinding(fullscreenBinding, e)) {
        e.preventDefault()
        void toggleFullscreen()
        return
      }
      if (matchesBinding(metaBinding, e)) {
        e.preventDefault()
        setMetaOpen((open) => !open)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [closeBinding, prevBinding, nextBinding, zoomInBinding, zoomOutBinding, fitBinding, actualBinding, fullscreenBinding, metaBinding, siblings, onNavigate, index, onClose, zoomBy, toggleFullscreen])

  // The hint fades out once the user has actually used the viewer.
  useEffect(() => {
    if (!hintVisible) return
    const t = window.setTimeout(() => setHintVisible(false), 6000)
    return () => window.clearTimeout(t)
  }, [hintVisible])

  const hasSiblings = !!siblings && siblings.length > 0 && index != null
  const currentIndex = index ?? 0
  const scalePercent = Math.round(view.scale * 100)
  const zoomLabel = mode === 'fit' ? '适应窗口' : mode === 'actual' ? '1:1' : `${scalePercent}%`

  const toggleFitActual = useCallback(() => {
    setMode((m) => (m === 'actual' ? 'fit' : 'actual'))
  }, [])

  return (
    <div className={`lightbox ${fullscreen ? 'is-fullscreen' : ''}`}>
      <div className="lightbox-stage">
        <div
          className={`lightbox-canvas ${dragging ? 'dragging' : ''}`}
          ref={stageRef}
          onClick={(e) => {
            // ONLY the bare backdrop closes the viewer. Clicking the image
            // has its own handler (below); every button has its own too.
            // Anything else must not close — a stray click on a control used
            // to throw the user out of the picture they were inspecting.
            if (suppressClickRef.current) return
            if (e.target !== e.currentTarget) return
            onClose()
          }}
        >
          <div
            className="lightbox-img-wrap"
            ref={wrapRef}
            onClick={() => {
              // Clicking the picture toggles 适应窗口 ⇄ 1:1.
              if (suppressClickRef.current) return
              toggleFitActual()
            }}
            style={{
              width: natural.w ? natural.w : undefined,
              height: natural.h ? natural.h : undefined,
              transform: `translate3d(${view.x - natural.w / 2}px, ${view.y - natural.h / 2}px, 0) scale(${view.scale})`,
            }}
          >
            <img
              ref={imgRef}
              src={src}
              alt={meta?.filename ?? 'preview'}
              draggable={false}
              onLoad={(e) => {
                const el = e.currentTarget
                if (el.naturalWidth && el.naturalHeight) {
                  const next = { w: el.naturalWidth, h: el.naturalHeight }
                  naturalRef.current = next
                  setNatural(next)
                }
              }}
            />
          </div>

          <div className="viewer-status" aria-live="polite">
            {hasSiblings && <span className="viewer-status-index">{currentIndex + 1} / {siblings.length}</span>}
            {natural.w > 0 && <span className="viewer-status-dim">{natural.w}×{natural.h}</span>}
            <span className="viewer-status-zoom">{zoomLabel}</span>
            {meta?.diffusion_model && (
              <span className="viewer-status-model" title={meta.diffusion_model}>{meta.diffusion_model}</span>
            )}
          </div>

          {hasSiblings && (
            <div className="viewer-nav">
              <button
                type="button"
                className="btn-tertiary viewer-nav-btn"
                disabled={currentIndex <= 0}
                onClick={(e) => { e.stopPropagation(); onNavigate?.(currentIndex - 1) }}
                title="上一张（←）"
                aria-label="上一张"
              >
                ‹
              </button>
              <button
                type="button"
                className="btn-tertiary viewer-nav-btn"
                disabled={currentIndex >= siblings.length - 1}
                onClick={(e) => { e.stopPropagation(); onNavigate?.(currentIndex + 1) }}
                title="下一张（→）"
                aria-label="下一张"
              >
                ›
              </button>
            </div>
          )}

          <div className="lightbox-hud">
            <button type="button" className="btn-tertiary" onClick={(e) => { e.stopPropagation(); zoomBy(1 / 1.25) }} title="缩小（-）">
              －
            </button>
            <span className="lb-scale">{scalePercent}%</span>
            <button type="button" className="btn-tertiary" onClick={(e) => { e.stopPropagation(); zoomBy(1.25) }} title="放大（+）">
              ＋
            </button>
            <button type="button" className="btn-tertiary" onClick={(e) => { e.stopPropagation(); setMode('fit') }} title="适应窗口（0）">
              适应窗口
            </button>
            <button type="button" className="btn-tertiary" onClick={(e) => { e.stopPropagation(); setMode('actual') }} title="实际像素（1）">
              1:1
            </button>
            {!isWebDev() && (
              <button
                type="button"
                className="btn-tertiary"
                onClick={(e) => { e.stopPropagation(); void toggleFullscreen() }}
                title="全屏（F）"
              >
                {fullscreen ? '退出全屏' : '全屏'}
              </button>
            )}
            <button type="button" className="btn-tertiary" onClick={(e) => { e.stopPropagation(); onClose() }} title="关闭（Esc）">
              关闭
            </button>
          </div>

          {hintVisible && (
            <p className="muted hint lb-hint">
              单击图片切换 适应窗口/1:1 · 滚轮以光标为锚缩放 · 拖动平移 · ←/→ 换图 · F 全屏 · M 元数据 · Esc 关闭
            </p>
          )}
        </div>
        {hasSiblings && (
          <FilmStrip
            images={siblings}
            activeIndex={currentIndex}
            onSelect={(i) => onNavigate?.(i)}
          />
        )}
      </div>
      {meta && (
        <aside className={`lightbox-side ${metaOpen ? 'open' : ''}`} onClick={(e) => e.stopPropagation()}>
          <button
            type="button"
            className={`lightbox-side-toggle ${metaOpen ? 'open' : ''}`}
            onClick={() => setMetaOpen((open) => !open)}
            aria-expanded={metaOpen}
            aria-label={metaOpen ? '收起 Prompt / 元数据' : '展开 Prompt / 元数据'}
            title={metaOpen ? '收起 Prompt / 元数据' : '展开 Prompt / 元数据'}
          >
            <span className="lightbox-side-label">{metaOpen ? '收起 Prompt / 元数据 ▾' : 'Prompt / 元数据'}</span>
            {meta.manually_grouped && (
              <span className={`manual-badge manual-${meta.manually_grouped}`}>
                {meta.manually_grouped === 'split' ? '本图已手动拆出为新分组' : '本组为手动合并'}
              </span>
            )}
          </button>
          <AnimatePresence initial={false}>
            {metaOpen && (
              <motion.div
                className="lightbox-side-panel"
                initial={{ opacity: 0, x: 18 }}
                animate={{ opacity: 1, x: 0 }}
                exit={{ opacity: 0, x: 18 }}
                transition={{ duration: 0.16, ease: 'easeOut' }}
              >
                <ImageMetaPopover img={meta} onMetaUpdated={onMetaUpdated} />
              </motion.div>
            )}
          </AnimatePresence>
        </aside>
      )}
    </div>
  )
}
