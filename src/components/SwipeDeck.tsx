import { useEffect, useCallback, useMemo, useRef, useState } from 'react'
import { motion, type PanInfo } from 'framer-motion'
import {
  arenaSuggested,
  assetUrl,
  listGroupImages,
  listLabels,
  setImageLabel,
  swipe as swipeApi,
  toggleHidden as toggleHiddenApi,
} from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'
import { bindingDisplay, matchesBinding } from '../keymap'
import { Lightbox } from './Lightbox'
import { Popover } from './Popover'
import { ImageMetaPopover } from './ImageMetaPopover'

const GESTURES = ['left', 'right', 'up', 'down'] as const
type Gesture = (typeof GESTURES)[number]

// Fly-off displacement per gesture. Tinder-style: left/right to fling
// horizontally, up/down to fling vertically. The card animates to this
// offset + opacity 0 then advances to the next card.
const EXIT_VEC: Record<Gesture, { x: number; y: number }> = {
  left: { x: -1400, y: 0 },
  right: { x: 1400, y: 0 },
  up: { x: 0, y: -1000 },
  down: { x: 0, y: 1000 },
}

// Fisher-Yates (Knuth) shuffle: uniform, in-place, O(n).
function shuffle<T>(arr: T[]): T[] {
  const a = [...arr]
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1))
    ;[a[i], a[j]] = [a[j], a[i]]
  }
  return a
}

export function SwipeDeck() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const keybindings = useStore((s) => s.keybindings)
  const labels = useStore((s) => s.labels)
  const setLabels = useStore((s) => s.setLabels)
  const [images, setImages] = useState<ImageRow[]>([])
  const [idx, setIdx] = useState(0)
  // When hiding a card we let the fly-off animation finish with the card
  // still present (so `current` keeps its identity), then on
  // onAnimationComplete we drop it from the deck. Because the element is
  // removed rather than the cursor advanced, Backspace rewind and replay can
  // never surface it again within this session.
  const hiddenPendingRef = useRef<number | null>(null)
  const [scores, setScores] = useState<Record<number, number>>({})
  const [arenaHint, setArenaHint] = useState<{ left: number; right: number } | null>(null)
  const [busy, setBusy] = useState(false)
  // Current fly-off vector; while non-null the top card is animating out
  // and gesture inputs are ignored. onAnimationComplete clears it and
  // advances the deck.
  const [exitVec, setExitVec] = useState<{ x: number; y: number } | null>(null)
  // Latest gesture that fired, to drive the matching button's flash. Keyed
  // off a monotonic counter so repeated same-direction gestures re-trigger
  // the CSS animation via React `key` remount.
  const [flash, setFlash] = useState<{ g: Gesture; n: number } | null>(null)
  const [lightbox, setLightbox] = useState<string | null>(null)
  const [popoverOpen, setPopoverOpen] = useState(false)
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    if (currentGroupKey == null) return
    listGroupImages(currentGroupKey, granularity).then((imgs) => {
      // Shuffle on entry so every visit (and every "重新过一遍") re-randomizes
      // the order instead of replaying the deterministic seed/filename sort.
      setImages(shuffle(imgs))
      hiddenPendingRef.current = null
      setIdx(0)
      setExitVec(null)
      setBusy(false)
      const sc: Record<number, number> = {}
      imgs.forEach((i) => {
        if (i.score != null) sc[i.id] = i.score
      })
      setScores(sc)
    })
    if (labels.length === 0) listLabels().then(setLabels)
  }, [currentGroupKey, granularity, labels.length, setLabels])

  const current = images[idx]
  const next = images[idx + 1]

  // Trigger a verdict: kick off the fly-off animation, flash the button,
  // and fire the score/label/suggestion IPCs in the background. Visual
  // advance is decoupled from IPC success — onAnimationComplete advances
  // the deck regardless. Wrapped so a thrown IPC can never leak `busy`.
  const applyGesture = useCallback(
    async (gesture: Gesture) => {
      if (!current || busy || exitVec) return
      setBusy(true)
      setExitVec(EXIT_VEC[gesture])
      setFlash({ g: gesture, n: (flash?.n ?? 0) + 1 })
      if (flashTimer.current) clearTimeout(flashTimer.current)
      flashTimer.current = setTimeout(() => setFlash(null), 320)
      try {
        const label = labels.find((l) => l.gesture === gesture)
        if (label) {
          await setImageLabel(current.id, label.id, true).catch(() => {})
        }
        if (gesture !== 'down') {
          const ns = await swipeApi(current.id, gesture).catch(() => null)
          if (ns != null) setScores((s) => ({ ...s, [current.id]: ns }))
        }
        if (next) {
          const suggested = await arenaSuggested(current.id, next.id).catch(() => false)
          if (suggested) setArenaHint({ left: current.id, right: next.id })
        }
      } finally {
        // busy is also released by onAnimationComplete as a safety net; we
        // intentionally don't release here so a slow IPC can't unblock the
        // guard before the fly-off finishes (double-firing would re-enter
        // applyGesture while the card is mid-animation).
      }
    },
    [current, next, busy, exitVec, labels, flash],
  )

  // keyboard — Tinder-style: left = 差 / right = 优 / up = 待优化 / down = 跳过.
  // All bindings come from the persisted keymap (Settings → 键位设置).
  // preventDefault so keys don't scroll the page. Backspace rewinds the
  // cursor (does NOT roll back the already-committed score). Hide blocks the
  // current card (score pinned to 0, advances to the next card).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (lightbox) return
      if (matchesBinding(keybindings.swipeLeft, e)) {
        e.preventDefault()
        applyGesture('left')
      } else if (matchesBinding(keybindings.swipeRight, e)) {
        e.preventDefault()
        applyGesture('right')
      } else if (matchesBinding(keybindings.swipeUp, e)) {
        e.preventDefault()
        applyGesture('up')
      } else if (matchesBinding(keybindings.swipeDown, e)) {
        e.preventDefault()
        applyGesture('down')
      } else if (matchesBinding(keybindings.swipeHide, e)) {
        e.preventDefault()
        hideCard()
      } else if (matchesBinding(keybindings.swipeRewind, e)) {
        e.preventDefault()
        setIdx((i) => Math.max(0, i - 1))
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [applyGesture, lightbox, keybindings])

  // Hide the current card: pin its score to 0 (done backend-side), flag it
  // for removal after the fly-off, and fly it away like a swipe so the deck
  // advances. Visual advance is decoupled from IPC success — onAnimationComplete
  // removes the card regardless.
  const hideCard = useCallback(async () => {
    if (!current || busy || exitVec) return
    setBusy(true)
    setPopoverOpen(false)
    setExitVec(EXIT_VEC.down)
    hiddenPendingRef.current = current.id
    try {
      await toggleHiddenApi(current.id, true).catch(() => {})
      setScores((s) => ({ ...s, [current.id]: 0 }))
    } finally {
      // busy released by onAnimationComplete (same contract as applyGesture)
    }
  }, [current, busy, exitVec])

  function onDragEnd(_: unknown, info: PanInfo) {
    const { offset, velocity } = info
    const threshold = 100
    if (offset.x < -threshold || velocity.x < -500) applyGesture('left')
    else if (offset.x > threshold || velocity.x > 500) applyGesture('right')
    else if (offset.y < -threshold || velocity.y < -500) applyGesture('up')
    else if (offset.y > threshold || velocity.y > 500) applyGesture('down')
  }

  const progress = useMemo(() => {
    if (images.length === 0) return 0
    return Math.round((idx / images.length) * 100)
  }, [idx, images.length])

  if (currentGroupKey == null) {
    return (
      <div className="panel">
        <p>未选择分组。</p>
        <button onClick={() => setView('groups')}>返回</button>
      </div>
    )
  }

  if (images.length === 0) {
    return (
      <div className="panel">
        <p className="muted">该组无图片。</p>
        <button onClick={() => setView('groups')}>返回分组</button>
      </div>
    )
  }

  if (idx >= images.length) {
    return (
      <div className="panel">
        <h2>本组已完成 ✓</h2>
        <p>已标注 {images.length} 张。</p>
        {arenaHint && (
          <div className="arena-hint">
            <p>检测到评分接近的卡片，可进入擂台模式精修：</p>
            <button onClick={() => setView('arena')}>进入擂台模式</button>
          </div>
        )}
        <div className="row">
          <button onClick={() => setView('groups')}>返回分组</button>
          <button
            onClick={() => {
              hiddenPendingRef.current = null
              setImages((arr) => shuffle(arr))
              setIdx(0)
              setExitVec(null)
            }}
          >
            重新洗牌再过一遍
          </button>
        </div>
      </div>
    )
  }

  const score = scores[current.id] ?? 50

  return (
    <div className="swipe-view">
      <div className="swipe-topbar">
        <button onClick={() => setView('groups')}>← 返回</button>
        <div className="progress">
          <div className="progress-bar" style={{ width: `${progress}%` }} />
        </div>
        <span className="counter">
          {idx + 1}/{images.length}
        </span>
        <button onClick={() => setView('folder')}>文件夹视角</button>
        <button onClick={() => setView('arena')}>擂台模式</button>
      </div>

      <div className="deck">
        {next && (
          <motion.div className="card card-under" key={next.id}>
            <img src={assetUrl(next.abs_path)} alt={next.filename} draggable={false} />
          </motion.div>
        )}
        <motion.div
          className="card"
          key={current.id}
          drag
          dragSnapToOrigin={false}
          onDragEnd={onDragEnd}
          whileDrag={{ scale: 1.04 }}
          animate={exitVec ? { ...exitVec, opacity: 0 } : false}
          transition={{ type: 'tween', duration: 0.22, ease: 'easeIn' }}
          onAnimationComplete={() => {
            if (!exitVec) return
            setExitVec(null)
            const hiddenId = hiddenPendingRef.current
            if (hiddenId != null) {
              // Hiding: drop the card from the deck instead of advancing the
              // cursor, so the deck naturally moves to the next card.
              hiddenPendingRef.current = null
              setImages((arr) => arr.filter((i) => i.id !== hiddenId))
            } else {
              setIdx((i) => i + 1)
            }
            setBusy(false)
          }}
        >
          <img
            src={assetUrl(current.abs_path)}
            alt={current.filename}
            draggable={false}
            onClick={() => setLightbox(current.abs_path)}
          />
          <Popover
            open={popoverOpen}
            onOpenChange={setPopoverOpen}
            trigger={<span className="more-btn">更多 ▾</span>}
          >
            <ImageMetaPopover img={current} onHide={hideCard} />
          </Popover>
          <div className="card-badge">
            <span className="score">{score.toFixed(0)}</span>
            <span className="seed">seed: {current.seed}</span>
          </div>
        </motion.div>
      </div>

      {/* Cross keypad: up = 待优化, mid = 差 / [empty] / 优, down = 跳过.
          Layout matches arrow-key directions so muscle memory from the
          keyboard carries straight over. */}
      <div className="gesture-cross">
        <div className="gesture-row">
          <button
            className="gesture-up ghost"
            onClick={() => applyGesture('up')}
            key={flash?.g === 'up' ? `up-${flash.n}` : 'up'}
            data-flash={flash?.g === 'up' ? '1' : undefined}
          >
            {labels.find((l) => l.gesture === 'up')?.name ?? '待优化'}
            <span className="kbd">{bindingDisplay(keybindings.swipeUp)}</span>
          </button>
        </div>
        <div className="gesture-row">
          <button
            className="gesture-left"
            onClick={() => applyGesture('left')}
            key={flash?.g === 'left' ? `left-${flash.n}` : 'left'}
            data-flash={flash?.g === 'left' ? '1' : undefined}
          >
            {labels.find((l) => l.gesture === 'left')?.name ?? '差'}
            <span className="kbd">{bindingDisplay(keybindings.swipeLeft)}</span>
          </button>
          <span className="gesture-center" aria-hidden />
          <button
            className="gesture-right"
            onClick={() => applyGesture('right')}
            key={flash?.g === 'right' ? `right-${flash.n}` : 'right'}
            data-flash={flash?.g === 'right' ? '1' : undefined}
          >
            {labels.find((l) => l.gesture === 'right')?.name ?? '优'}
            <span className="kbd">{bindingDisplay(keybindings.swipeRight)}</span>
          </button>
        </div>
        <div className="gesture-row">
          <button
            className="gesture-down ghost"
            onClick={() => applyGesture('down')}
            key={flash?.g === 'down' ? `down-${flash.n}` : 'down'}
            data-flash={flash?.g === 'down' ? '1' : undefined}
          >
            {labels.find((l) => l.gesture === 'down')?.name ?? '跳过'}
            <span className="kbd">{bindingDisplay(keybindings.swipeDown)}</span>
          </button>
        </div>
      </div>
      <div className="gesture-extra-row">
        <button
          className="gesture-hide"
          onClick={hideCard}
          disabled={busy}
          title="屏蔽（赋 0 分，不参与评分，可在文件夹视角恢复）"
        >
          屏蔽 <span className="kbd">{bindingDisplay(keybindings.swipeHide)}</span>
        </button>
      </div>
      <p className="muted hint">
        键盘 {bindingDisplay(keybindings.swipeLeft)} {bindingDisplay(keybindings.swipeRight)}{' '}
        {bindingDisplay(keybindings.swipeUp)} {bindingDisplay(keybindings.swipeDown)} 触发判定 ·{' '}
        {bindingDisplay(keybindings.swipeHide)} 屏蔽当前卡 ·{' '}
        {bindingDisplay(keybindings.swipeRewind)} 回退游标 · 单击图片放大 · "更多"查看 Prompt / 屏蔽
      </p>

      {lightbox && <Lightbox src={assetUrl(lightbox)} onClose={() => setLightbox(null)} />}
    </div>
  )
}