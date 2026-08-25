import { useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion, type PanInfo } from 'framer-motion'
import { useStore } from '../store'
import { matchesBinding } from '../keymap'
import type { ImageRow } from '../types'
import { ImageMetaPopover } from './ImageMetaPopover'

// Minimal lightbox mirroring the conventions users already expect from
// image viewers: click to zoom-to-fit, wheel to scale, drag to pan, ESC
// or backdrop click to close. Goals: low cognitive overhead — same as
// popular image browsers (e.g. Windows Photos, Honeyview).
//
// Layout: a flex row — the image stage always gets the whole viewport minus
// the side rail; when `meta` is provided (FolderView) a right sidebar exists
// whose toggle summons the metadata panel. Metadata NEVER overlays the
// zoomed image: opening an image for a full look always yields an
// unobstructed image.
interface Props {
  src: string
  onClose: () => void
  /** Optional: when provided, render a right sidebar with a collapsible
   *  metadata panel (prompt / model / seed…) so a zoomed image can also be
   *  inspected — used by FolderView where there is no other way to see an
   *  image's metadata. */
  meta?: ImageRow
}

const MIN = 0.1
const MAX = 10
const FIT = 1

export function Lightbox({ src, onClose, meta }: Props) {
  const [scale, setScale] = useState(FIT)
  const [pos, setPos] = useState({ x: 0, y: 0 })
  const [metaOpen, setMetaOpen] = useState(false)
  const wheelAccum = useRef(0)
  const closeBinding = useStore((s) => s.keybindings.closeLightbox)

  // Close key (default Esc) closes; reset transform on unmount so a future
  // open starts clean.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (matchesBinding(closeBinding, e)) {
        e.preventDefault()
        onClose()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose, closeBinding])

  function clamp(v: number, lo: number, hi: number) {
    return Math.max(lo, Math.min(hi, v))
  }

  function onWheel(e: React.WheelEvent) {
    // debounce tiny deltas (trackpads) with a small accumulator
    wheelAccum.current += e.deltaY
    if (Math.abs(wheelAccum.current) < 8) return
    const step = wheelAccum.current > 0 ? -0.15 : 0.15
    wheelAccum.current = 0
    setScale((s) => {
      return clamp(s + step * Math.max(s, 0.5), MIN, MAX)
    })
  }

  function onPanEnd(_: unknown, info: PanInfo) {
    setPos((p) => ({ x: p.x + info.offset.x, y: p.y + info.offset.y }))
  }

  function handleClick(e: React.MouseEvent) {
    // 仅在点击背景层（非图片本体 / 侧栏）时关闭；图片本体单击切换 fit/2x。
    if (e.target === e.currentTarget) {
      onClose()
      return
    }
    setPos({ x: 0, y: 0 })
    setScale((s) => (s < 1.5 ? 2 : FIT))
  }

  return (
    <div className="lightbox">
      <div className="lightbox-stage" onClick={handleClick} onWheel={onWheel}>
        <motion.div
          className="lightbox-img-wrap"
          drag={scale > FIT}
          dragSnapToOrigin={false}
          onDragEnd={onPanEnd}
          animate={{ scale, x: pos.x, y: pos.y }}
          transition={{ type: 'spring', stiffness: 260, damping: 28 }}
          style={{ cursor: scale > FIT ? 'grab' : 'zoom-in' }}
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={() => {
            setPos({ x: 0, y: 0 })
            setScale((s) => (s > 1.0001 ? FIT : 2))
          }}
        >
          <img src={src} alt="preview" draggable={false} />
        </motion.div>
        <div className="lightbox-hud">
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation()
              setScale((s) => clamp(s - 0.2, MIN, MAX))
            }}
          >
            －
          </button>
          <span className="lb-scale">{Math.round(scale * 100)}%</span>
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation()
              setScale((s) => clamp(s + 0.2, MIN, MAX))
            }}
          >
            ＋
          </button>
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation()
              setPos({ x: 0, y: 0 })
              setScale(FIT)
            }}
          >
            适合
          </button>
          <button type="button" onClick={(e) => { e.stopPropagation(); onClose() }}>关闭</button>
        </div>
        <p className="muted hint lb-hint">单击切换 1×/2× · 双击重置 · 滚轮缩放 · 拖动平移 · ESC 关闭</p>
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
                <ImageMetaPopover img={meta} />
              </motion.div>
            )}
          </AnimatePresence>
        </aside>
      )}
    </div>
  )
}
