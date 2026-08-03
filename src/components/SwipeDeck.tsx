import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { motion, type PanInfo } from 'framer-motion'
import {
  applySwipeAction,
  arenaSuggested,
  assetUrl,
  errorMessage,
  listGroupImages,
  listLabels,
  toggleHiddenAction,
  undoReviewAction,
} from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'
import { bindingDisplay, matchesBinding } from '../keymap'
import { getTelemetrySessionId, trackAction } from '../telemetry'
import { Lightbox } from './Lightbox'
import { Popover } from './Popover'
import { ImageMetaPopover } from './ImageMetaPopover'

const GESTURES = ['left', 'right', 'up', 'down'] as const
type Gesture = (typeof GESTURES)[number]
type RetryAction = Gesture | 'hide' | 'undo'

function isEditableTarget(target: EventTarget | null) {
  return target instanceof HTMLElement && (
    target.matches('input, textarea, select, [contenteditable="true"]') ||
    target.closest('[contenteditable="true"]') != null
  )
}

const EXIT_VEC: Record<Gesture, { x: number; y: number }> = {
  left: { x: -1400, y: 0 },
  right: { x: 1400, y: 0 },
  up: { x: 0, y: -1000 },
  down: { x: 0, y: 1000 },
}

function shuffle<T>(arr: T[]): T[] {
  const result = [...arr]
  for (let i = result.length - 1; i > 0; i -= 1) {
    const j = Math.floor(Math.random() * (i + 1))
    ;[result[i], result[j]] = [result[j], result[i]]
  }
  return result
}

function restoreOrder(images: ImageRow[], order: number[]) {
  const byId = new Map(images.map((image) => [image.id, image]))
  const restored = order.flatMap((id) => {
    const image = byId.get(id)
    if (!image) return []
    byId.delete(id)
    return [image]
  })
  return [...restored, ...shuffle(Array.from(byId.values()))]
}

export function SwipeDeck() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const keybindings = useStore((s) => s.keybindings)
  const labels = useStore((s) => s.labels)
  const setLabels = useStore((s) => s.setLabels)
  const updateReviewSession = useStore((s) => s.updateReviewSession)
  const [images, setImages] = useState<ImageRow[]>([])
  const [idx, setIdx] = useState(0)
  const [scores, setScores] = useState<Record<number, number>>({})
  const [arenaHint, setArenaHint] = useState<{ left: number; right: number } | null>(null)
  const [status, setStatus] = useState<'loading' | 'ready' | 'error'>('loading')
  const [loadError, setLoadError] = useState('')
  const [actionError, setActionError] = useState('')
  const [retryAction, setRetryAction] = useState<RetryAction | null>(null)
  const [busy, setBusy] = useState(false)
  const [exitVec, setExitVec] = useState<{ x: number; y: number } | null>(null)
  const [flash, setFlash] = useState<{ g: Gesture; n: number } | null>(null)
  const [lightbox, setLightbox] = useState<string | null>(null)
  const [popoverOpen, setPopoverOpen] = useState(false)
  const hiddenPendingRef = useRef<number | null>(null)
  const loadRequestRef = useRef(0)
  const actionRequestRef = useRef(0)
  const actionInFlightRef = useRef(false)
  const reloadAfterActionRef = useRef(false)
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const startedAtRef = useRef(new Date().toISOString())

  const load = useCallback(async () => {
    if (currentGroupKey == null) return
    if (actionInFlightRef.current) {
      reloadAfterActionRef.current = true
      return
    }
    const request = ++loadRequestRef.current
    setStatus('loading')
    setLoadError('')
    setActionError('')
    setBusy(false)
    setExitVec(null)
    hiddenPendingRef.current = null
    try {
      const [loadedImages, loadedLabels] = await Promise.all([
        listGroupImages(currentGroupKey, granularity),
        labels.length > 0 ? Promise.resolve(labels) : listLabels(),
      ])
      if (request !== loadRequestRef.current) return
      if (labels.length === 0) setLabels(loadedLabels)
      const saved = useStore.getState().reviewSession
      const canRestore = saved.groupKey === currentGroupKey && saved.granularity === granularity
      const ordered = canRestore ? restoreOrder(loadedImages, saved.swipeOrder) : shuffle(loadedImages)
      const cursor = canRestore ? Math.min(saved.swipeCursor, ordered.length) : 0
      const nextScores: Record<number, number> = {}
      loadedImages.forEach((image) => {
        if (image.score != null) nextScores[image.id] = image.score
      })
      setImages(ordered)
      setIdx(cursor)
      setScores(nextScores)
      updateReviewSession({
        mode: 'swipe',
        groupKey: currentGroupKey,
        granularity,
        swipeOrder: ordered.map((image) => image.id),
        swipeCursor: cursor,
      })
      setStatus('ready')
    } catch (error) {
      if (request !== loadRequestRef.current) return
      setLoadError(errorMessage(error))
      setStatus('error')
    }
  }, [currentGroupKey, granularity, labels, setLabels, updateReviewSession])

  useEffect(() => {
    void load()
    return () => {
      loadRequestRef.current += 1
      if (flashTimer.current) clearTimeout(flashTimer.current)
    }
  }, [load])

  const current = images[idx]
  const next = images[idx + 1]

  useEffect(() => {
    if (current) startedAtRef.current = new Date().toISOString()
  }, [current?.id])

  const applyGesture = useCallback(async (gesture: Gesture) => {
    if (!current || busy || exitVec) return
    const request = ++actionRequestRef.current
    const startedAt = startedAtRef.current
    const startedMs = Date.now()
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    setFlash({ g: gesture, n: (flash?.n ?? 0) + 1 })
    if (flashTimer.current) clearTimeout(flashTimer.current)
    flashTimer.current = setTimeout(() => setFlash(null), 320)
    try {
      const label = labels.find((item) => item.gesture === gesture)
      const result = await applySwipeAction(current.id, gesture, label?.id, {
        sessionId: getTelemetrySessionId(),
        startedAt,
        contextSignature: currentGroupKey ?? undefined,
      })
      if (request !== actionRequestRef.current) return
      const session = useStore.getState().reviewSession
      const undoStack = session.swipeUndoStack ?? []
      updateReviewSession({
        swipeUndoStack: [...undoStack, {
          actionId: result.action_id,
          imageId: current.id,
          index: idx,
          kind: 'swipe' as const,
        }].slice(-100),
      })
      setScores((value) => ({ ...value, [current.id]: result.score }))
      trackAction('swipe_commit', {
        gesture,
        has_label: Boolean(label),
        duration_ms: Date.now() - startedMs,
        image_id: current.id,
      })
      setExitVec(EXIT_VEC[gesture])
      if (next) {
        void arenaSuggested(current.id, next.id).then((suggested) => {
          if (suggested) setArenaHint({ left: current.id, right: next.id })
        }).catch(() => {})
      }
    } catch (error) {
      if (request !== actionRequestRef.current) return
      setExitVec(null)
      actionInFlightRef.current = false
      setBusy(false)
      setRetryAction(gesture)
      setActionError(errorMessage(error))
    }
  }, [busy, current, currentGroupKey, exitVec, flash?.n, idx, labels, next, updateReviewSession])

  const hideCard = useCallback(async () => {
    if (!current || busy || exitVec) return
    const request = ++actionRequestRef.current
    const startedMs = Date.now()
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    setPopoverOpen(false)
    try {
      const result = await toggleHiddenAction(current.id, true, {
        sessionId: getTelemetrySessionId(),
        startedAt: startedAtRef.current,
        contextSignature: currentGroupKey ?? undefined,
      })
      if (request !== actionRequestRef.current) return
      const session = useStore.getState().reviewSession
      const undoStack = session.swipeUndoStack ?? []
      updateReviewSession({
        swipeUndoStack: [...undoStack, {
          actionId: result.action_id,
          imageId: current.id,
          index: idx,
          kind: 'hide' as const,
        }].slice(-100),
      })
      hiddenPendingRef.current = current.id
      setScores((value) => ({ ...value, [current.id]: result.score }))
      trackAction('hide', {
        hidden: true,
        mode: 'swipe',
        duration_ms: Date.now() - startedMs,
        image_id: current.id,
      })
      setExitVec(EXIT_VEC.down)
    } catch (error) {
      if (request !== actionRequestRef.current) return
      hiddenPendingRef.current = null
      setExitVec(null)
      actionInFlightRef.current = false
      setBusy(false)
      setRetryAction('hide')
      setActionError(errorMessage(error))
    }
  }, [busy, current, currentGroupKey, exitVec, idx, updateReviewSession])

  const undoLastAction = useCallback(async () => {
    if (busy || exitVec) return
    const session = useStore.getState().reviewSession
    const undoStack = session.swipeUndoStack ?? []
    const entry = undoStack[undoStack.length - 1]
    if (!entry) return
    const request = ++actionRequestRef.current
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    try {
      const result = await undoReviewAction(entry.actionId, getTelemetrySessionId())
      if (request !== actionRequestRef.current) return
      const nextStack = undoStack.slice(0, -1)
      if (entry.kind === 'hide' && currentGroupKey != null) {
        const loaded = await listGroupImages(currentGroupKey, granularity)
        if (request !== actionRequestRef.current) return
        const order = useStore.getState().reviewSession.swipeOrder
        const ordered = restoreOrder(loaded, order)
        const nextScores: Record<number, number> = {}
        loaded.forEach((image) => {
          if (image.score != null) nextScores[image.id] = image.score
        })
        setImages(ordered)
        setScores(nextScores)
        updateReviewSession({
          swipeOrder: ordered.map((image) => image.id),
          swipeCursor: entry.index,
          swipeUndoStack: nextStack,
        })
      } else {
        setScores((value) => {
          const nextScores = { ...value }
          if (result.restored_score == null) delete nextScores[result.image_id]
          else nextScores[result.image_id] = result.restored_score
          return nextScores
        })
        updateReviewSession({
          swipeCursor: entry.index,
          swipeUndoStack: nextStack,
        })
      }
      setIdx(entry.index)
      hiddenPendingRef.current = null
      setExitVec(null)
      actionInFlightRef.current = false
      setBusy(false)
    } catch (error) {
      if (request !== actionRequestRef.current) return
      actionInFlightRef.current = false
      setBusy(false)
      setRetryAction('undo')
      setActionError(errorMessage(error))
    }
  }, [busy, currentGroupKey, exitVec, granularity, updateReviewSession])

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (lightbox || popoverOpen) return
      if (isEditableTarget(event.target)) return
      if (matchesBinding(keybindings.swipeLeft, event)) {
        event.preventDefault()
        void applyGesture('left')
      } else if (matchesBinding(keybindings.swipeRight, event)) {
        event.preventDefault()
        void applyGesture('right')
      } else if (matchesBinding(keybindings.swipeUp, event)) {
        event.preventDefault()
        void applyGesture('up')
      } else if (matchesBinding(keybindings.swipeDown, event)) {
        event.preventDefault()
        void applyGesture('down')
      } else if (matchesBinding(keybindings.swipeHide, event)) {
        event.preventDefault()
        void hideCard()
      } else if (matchesBinding(keybindings.swipeRewind, event) && !busy) {
        event.preventDefault()
        void undoLastAction()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [applyGesture, busy, hideCard, keybindings, lightbox, popoverOpen, undoLastAction])

  function onDragEnd(_: unknown, info: PanInfo) {
    const { offset, velocity } = info
    const threshold = 100
    if (offset.x < -threshold || velocity.x < -500) void applyGesture('left')
    else if (offset.x > threshold || velocity.x > 500) void applyGesture('right')
    else if (offset.y < -threshold || velocity.y < -500) void applyGesture('up')
    else if (offset.y > threshold || velocity.y > 500) void applyGesture('down')
  }

  const progress = useMemo(
    () => images.length === 0 ? 0 : Math.round((idx / images.length) * 100),
    [idx, images.length],
  )

  if (currentGroupKey == null) {
    return <StatePanel message="未选择分组。" actionLabel="返回分组" onAction={() => setView('groups')} />
  }
  if (status === 'loading') return <StatePanel message="正在加载滑卡队列…" />
  if (status === 'error') {
    return <StatePanel message={`加载失败：${loadError}`} actionLabel="重试" onAction={() => void load()} />
  }
  if (images.length === 0) {
    return <StatePanel message="该组无可评分图片。" actionLabel="返回分组" onAction={() => setView('groups')} />
  }
  if (idx >= images.length) {
    return (
      <div className="panel">
        <h2>本组已完成</h2>
        <p>已标注 {images.length} 张。</p>
        {arenaHint && (
          <button
            type="button"
            className="arena-hint arena-hint-glow"
            aria-label="进入擂台模式"
            title="进入擂台模式"
            onClick={() => setView('arena')}
          />
        )}
        <div className="row">
          <button type="button" onClick={() => setView('groups')}>返回分组</button>
          <button
            type="button"
            onClick={() => {
              const ordered = shuffle(images)
              setImages(ordered)
              setIdx(0)
              setExitVec(null)
              updateReviewSession({
                swipeOrder: ordered.map((image) => image.id),
                swipeCursor: 0,
                swipeUndoStack: [],
              })
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
        <button type="button" onClick={() => setView('groups')}>← 返回</button>
        <div className="progress"><div className="progress-bar" style={{ width: `${progress}%` }} /></div>
        <span className="counter">{idx + 1}/{images.length}</span>
        <button type="button" onClick={() => setView('folder')}>文件夹视角</button>
        <button type="button" onClick={() => setView('arena')}>擂台模式</button>
      </div>

      {actionError && (
        <div className="action-error" role="alert">
          <span>{actionError}</span>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              if (retryAction === 'hide') void hideCard()
              else if (retryAction === 'undo') void undoLastAction()
              else if (retryAction) void applyGesture(retryAction)
            }}
          >
            重试
          </button>
        </div>
      )}

      <div className="deck">
        {next && <motion.div className="card card-under" key={next.id}><img src={assetUrl(next.abs_path)} alt={next.filename} draggable={false} /></motion.div>}
        <motion.div
          className="card"
          key={current.id}
          drag={!busy}
          dragSnapToOrigin
          onDragEnd={onDragEnd}
          whileDrag={{ scale: 1.04 }}
          animate={exitVec ? { ...exitVec, opacity: 0 } : { x: 0, y: 0, opacity: 1 }}
          transition={{ type: 'tween', duration: 0.22, ease: 'easeIn' }}
          onAnimationComplete={() => {
            if (!exitVec) return
            setExitVec(null)
            const hiddenId = hiddenPendingRef.current
            if (hiddenId != null) {
              hiddenPendingRef.current = null
              setImages((value) => {
                const nextImages = value.filter((image) => image.id !== hiddenId)
                updateReviewSession({ swipeCursor: idx })
                return nextImages
              })
            } else {
              const nextCursor = idx + 1
              setIdx(nextCursor)
              updateReviewSession({ swipeCursor: nextCursor })
            }
            actionInFlightRef.current = false
            setBusy(false)
            if (reloadAfterActionRef.current) {
              reloadAfterActionRef.current = false
              void load()
            }
          }}
        >
          <img src={assetUrl(current.abs_path)} alt={current.filename} draggable={false} onClick={() => setLightbox(current.abs_path)} />
          <Popover
            open={popoverOpen}
            onOpenChange={setPopoverOpen}
            trigger={<button type="button" className="more-btn">更多 ▾</button>}
          >
            <ImageMetaPopover img={current} onHide={hideCard} />
          </Popover>
          <div className="card-badge">
            <span className="score">{score.toFixed(0)}</span>
            <span className="seed">seed: {current.seed}</span>
          </div>
        </motion.div>
      </div>

      <div className="gesture-cross">
        <div className="gesture-row">
          <GestureButton gesture="up" label={labels.find((item) => item.gesture === 'up')?.name ?? '待优化'} binding={bindingDisplay(keybindings.swipeUp)} flash={flash} busy={busy} onClick={applyGesture} />
        </div>
        <div className="gesture-row">
          <GestureButton gesture="left" label={labels.find((item) => item.gesture === 'left')?.name ?? '差'} binding={bindingDisplay(keybindings.swipeLeft)} flash={flash} busy={busy} onClick={applyGesture} />
          <span className="gesture-center" aria-hidden />
          <GestureButton gesture="right" label={labels.find((item) => item.gesture === 'right')?.name ?? '优'} binding={bindingDisplay(keybindings.swipeRight)} flash={flash} busy={busy} onClick={applyGesture} />
        </div>
        <div className="gesture-row">
          <GestureButton gesture="down" label={labels.find((item) => item.gesture === 'down')?.name ?? '跳过'} binding={bindingDisplay(keybindings.swipeDown)} flash={flash} busy={busy} onClick={applyGesture} />
        </div>
      </div>
      <div className="gesture-extra-row">
        <button type="button" className="gesture-hide" onClick={() => void hideCard()} disabled={busy} title="屏蔽（赋 0 分，不参与评分，可在文件夹视角恢复）">
          屏蔽 <span className="kbd">{bindingDisplay(keybindings.swipeHide)}</span>
        </button>
      </div>
      <p className="muted hint">
        键盘 {bindingDisplay(keybindings.swipeLeft)} {bindingDisplay(keybindings.swipeRight)}{' '}
        {bindingDisplay(keybindings.swipeUp)} {bindingDisplay(keybindings.swipeDown)} 触发判定 ·{' '}
        {bindingDisplay(keybindings.swipeHide)} 屏蔽当前卡 ·{' '}
        {bindingDisplay(keybindings.swipeRewind)} 撤销上一步 · 单击图片放大 · "更多"查看 Prompt / 屏蔽
      </p>
      {lightbox && <Lightbox src={assetUrl(lightbox)} onClose={() => setLightbox(null)} />}
    </div>
  )
}

function StatePanel({ message, actionLabel, onAction }: { message: string; actionLabel?: string; onAction?: () => void }) {
  return (
    <div className="panel state-panel" role="status">
      <p className="muted">{message}</p>
      {actionLabel && onAction && <button type="button" onClick={onAction}>{actionLabel}</button>}
    </div>
  )
}

function GestureButton({
  gesture,
  label,
  binding,
  flash,
  busy,
  onClick,
}: {
  gesture: Gesture
  label: string
  binding: string
  flash: { g: Gesture; n: number } | null
  busy: boolean
  onClick: (gesture: Gesture) => Promise<void>
}) {
  return (
    <button
      type="button"
      className={`gesture-${gesture} ${gesture === 'up' || gesture === 'down' ? 'ghost' : ''}`}
      onClick={() => void onClick(gesture)}
      disabled={busy}
      key={flash?.g === gesture ? `${gesture}-${flash.n}` : gesture}
      data-flash={flash?.g === gesture ? '1' : undefined}
    >
      {label}<span className="kbd">{binding}</span>
    </button>
  )
}
