import { useEffect, useCallback, useMemo, useState } from 'react'
import { motion, type PanInfo } from 'framer-motion'
import {
  arenaSuggested,
  assetUrl,
  listGroupImages,
  listLabels,
  setImageLabel,
  swipe as swipeApi,
} from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'

const GESTURES = ['left', 'right', 'up', 'down'] as const
type Gesture = (typeof GESTURES)[number]

export function SwipeDeck() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const labels = useStore((s) => s.labels)
  const setLabels = useStore((s) => s.setLabels)
  const [images, setImages] = useState<ImageRow[]>([])
  const [idx, setIdx] = useState(0)
  const [scores, setScores] = useState<Record<number, number>>({})
  const [arenaHint, setArenaHint] = useState<{ left: number; right: number } | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (currentGroupKey == null) return
    listGroupImages(currentGroupKey, granularity).then((imgs) => {
      setImages(imgs)
      setIdx(0)
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

  const applyGesture = useCallback(
    async (gesture: Gesture) => {
      if (!current || busy) return
      setBusy(true)
      const label = labels.find((l) => l.gesture === gesture)
      if (label) {
        await setImageLabel(current.id, label.id, true).catch(() => {})
      }
      if (gesture !== 'down') {
        const newScore = await swipeApi(current.id, gesture)
        setScores((s) => ({ ...s, [current.id]: newScore }))
      }
      // arena suggestion: compare with next
      if (next) {
        const suggested = await arenaSuggested(current.id, next.id).catch(() => false)
        if (suggested) {
          setArenaHint({ left: current.id, right: next.id })
        }
      }
      setIdx((i) => i + 1)
      setBusy(false)
    },
    [current, next, busy, labels, scores],
  )

  // keyboard
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'ArrowLeft') applyGesture('left')
      else if (e.key === 'ArrowRight') applyGesture('right')
      else if (e.key === 'ArrowUp') applyGesture('up')
      else if (e.key === 'ArrowDown') applyGesture('down')
      else if (e.key === 'Backspace') setIdx((i) => Math.max(0, i - 1))
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [applyGesture])

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
          <button onClick={() => setIdx(0)}>重新过一遍</button>
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
          dragSnapToOrigin
          onDragEnd={onDragEnd}
          whileDrag={{ scale: 1.02 }}
        >
          <img src={assetUrl(current.abs_path)} alt={current.filename} draggable={false} />
          <div className="card-badge">
            <span className="score">{score.toFixed(0)}</span>
            <span className="seed">seed: {current.seed}</span>
          </div>
        </motion.div>
      </div>

      <div className="swipe-meta">
        <details>
          <summary>Prompt / 元数据</summary>
          <div className="meta-grid">
            <div>
              <strong>正 prompt</strong>
              <pre>{current.prompt_pos || '(无)'}</pre>
            </div>
            <div>
              <strong>负 prompt</strong>
              <pre>{current.prompt_neg || '(无)'}</pre>
            </div>
            <div>
              <strong>模型</strong>
              <pre>{current.checkpoint || '(无)'}</pre>
            </div>
            <div>
              <strong>LoRA / VAE</strong>
              <pre>{current.loras || '(无)'}{"\n"}VAE: {current.vae || '(默认)'}</pre>
            </div>
          </div>
        </details>
      </div>

      <div className="gesture-bar">
        {GESTURES.map((g) => {
          const label = labels.find((l) => l.gesture === g)
          return (
            <button key={g} className={`gesture-${g}`} onClick={() => applyGesture(g)}>
              {label ? label.name : g}
            </button>
          )
        })}
      </div>
      <p className="muted hint">键盘 ← → ↑ ↓ / Backspace 撤销</p>
    </div>
  )
}
