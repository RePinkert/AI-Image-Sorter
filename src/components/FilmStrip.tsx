import { useEffect, useMemo, useRef } from 'react'
import { assetUrl } from '../api'
import type { ImageRow } from '../types'

// A horizontal filmstrip of the surrounding images, borrowed from the
// "preview pane" of a mature local photo manager.
//
// Two constraints shaped this component:
//   1. No thumbnail cache exists in this app — the originals ARE the images
//      the app serves. So the strip must not eagerly load hundreds of
//      originals: only a window around the active image is mounted, and the
//      browser's own lazy loading defers the rest.
//   2. The strip must never overlap the zoomed image (军规 17 / the
//      "unobstructed image" contract), so it lives BELOW the stage, not
//      floating on top of it.
interface FilmStripProps {
  images: ImageRow[]
  activeIndex: number
  onSelect: (index: number) => void
}

/** How many items are mounted around the active one. */
const WINDOW = 120
/** Items kept before the active one inside the window. */
const LEAD = 60

export function FilmStrip({ images, activeIndex, onSelect }: FilmStripProps) {
  const listRef = useRef<HTMLDivElement | null>(null)
  const activeRef = useRef<HTMLButtonElement | null>(null)

  const start = useMemo(() => {
    if (images.length <= WINDOW) return 0
    return Math.max(0, Math.min(activeIndex - LEAD, images.length - WINDOW))
  }, [images.length, activeIndex])

  const visible = useMemo(() => images.slice(start, start + WINDOW), [images, start])

  // Keep the active thumbnail in view when the user pages.
  useEffect(() => {
    const el = activeRef.current
    if (!el) return
    el.scrollIntoView({ block: 'nearest', inline: 'center' })
  }, [activeIndex, start])

  if (images.length === 0) return null

  return (
    <div className="filmstrip" ref={listRef} role="listbox" aria-label="同文件夹图片">
      {visible.map((img, i) => {
        const index = start + i
        const active = index === activeIndex
        return (
          <button
            key={img.id}
            ref={active ? activeRef : undefined}
            type="button"
            role="option"
            aria-selected={active}
            className={`filmstrip-item ${active ? 'active' : ''}`}
            onClick={() => onSelect(index)}
            title={img.filename}
          >
            <img src={assetUrl(img.abs_path)} alt="" loading="lazy" decoding="async" draggable={false} />
            <span className="filmstrip-label">{img.filename}</span>
          </button>
        )
      })}
    </div>
  )
}
