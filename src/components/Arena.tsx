import { useCallback, useEffect, useRef, useState } from 'react'
import { arenaHide, arenaVote, assetUrl, errorMessage, listGroupImages, undoReviewAction } from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'
import { bindingDisplay, matchesBinding } from '../keymap'
import { getTelemetrySessionId, trackAction } from '../telemetry'
import { Lightbox } from './Lightbox'
import { Popover } from './Popover'
import { ImageMetaPopover } from './ImageMetaPopover'

const ARENA_THRESHOLD = 5
const RECENT_LIMIT = 20

function pairKey(a: number, b: number) {
  return a < b ? `${a}:${b}` : `${b}:${a}`
}

function pairAll(images: ImageRow[]): [ImageRow, ImageRow][] {
  const result: [ImageRow, ImageRow][] = []
  for (let i = 0; i < images.length; i += 1) {
    for (let j = i + 1; j < images.length; j += 1) result.push([images[i], images[j]])
  }
  return result
}

function isEditableTarget(target: EventTarget | null) {
  return target instanceof HTMLElement && (
    target.matches('input, textarea, select, [contenteditable="true"]') ||
    target.closest('[contenteditable="true"]') != null
  )
}

type RetryAction =
  | { kind: 'vote'; winnerIsLeft: boolean }
  | { kind: 'hide'; image: ImageRow }
  | { kind: 'undo' }

export function Arena() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const keybindings = useStore((s) => s.keybindings)
  const reviewSession = useStore((s) => s.reviewSession)
  const updateReviewSession = useStore((s) => s.updateReviewSession)
  const [images, setImages] = useState<ImageRow[]>([])
  const [left, setLeft] = useState<ImageRow | null>(null)
  const [right, setRight] = useState<ImageRow | null>(null)
  const [scores, setScores] = useState<Record<number, number>>({})
  const [status, setStatus] = useState<'loading' | 'ready' | 'error'>('loading')
  const [loadError, setLoadError] = useState('')
  const [actionError, setActionError] = useState('')
  const [retryAction, setRetryAction] = useState<RetryAction | null>(null)
  const [busy, setBusy] = useState(false)
  const [lightbox, setLightbox] = useState<string | null>(null)
  const [fly, setFly] = useState<'left' | 'right' | null>(null)
  const [leftPopoverOpen, setLeftPopoverOpen] = useState(false)
  const [rightPopoverOpen, setRightPopoverOpen] = useState(false)
  const [pendingHide, setPendingHide] = useState(false)
  const recent = useRef<Set<string>>(new Set())
  const loadRequestRef = useRef(0)
  const actionRequestRef = useRef(0)
  const actionInFlightRef = useRef(false)
  const reloadAfterActionRef = useRef(false)
  const transitionTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const startedAtRef = useRef(new Date().toISOString())
  const bothOpen = leftPopoverOpen && rightPopoverOpen

  const selectPair = useCallback((nextImages: ImageRow[], nextScores: Record<number, number>) => {
    const closePairs = pairAll(nextImages).filter(([a, b]) =>
      Math.abs((nextScores[a.id] ?? 50) - (nextScores[b.id] ?? 50)) < ARENA_THRESHOLD,
    )
    const pool = closePairs.length > 0 ? closePairs : pairAll(nextImages)
    const fresh = pool.filter(([a, b]) => !recent.current.has(pairKey(a.id, b.id)))
    const choices = fresh.length > 0 ? fresh : pool
    if (choices.length === 0) {
      setLeft(null)
      setRight(null)
      updateReviewSession({ arenaPair: null })
      return
    }
    const [nextLeft, nextRight] = choices[Math.floor(Math.random() * choices.length)]
    recent.current.add(pairKey(nextLeft.id, nextRight.id))
    if (recent.current.size > RECENT_LIMIT) {
      const entries = Array.from(recent.current)
      recent.current = new Set(entries.slice(Math.floor(entries.length / 2)))
    }
    setLeft(nextLeft)
    setRight(nextRight)
    startedAtRef.current = new Date().toISOString()
    updateReviewSession({ arenaPair: [nextLeft.id, nextRight.id] })
  }, [updateReviewSession])

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
    setFly(null)
    try {
      const loaded = await listGroupImages(currentGroupKey, granularity)
      if (request !== loadRequestRef.current) return
      const nextScores: Record<number, number> = {}
      loaded.forEach((image) => {
        if (image.score != null) nextScores[image.id] = image.score
      })
      setImages(loaded)
      setScores(nextScores)
      const saved = useStore.getState().reviewSession
      const savedPair = saved.groupKey === currentGroupKey && saved.granularity === granularity
        ? saved.arenaPair
        : null
      const restoredLeft = savedPair ? loaded.find((image) => image.id === savedPair[0]) : undefined
      const restoredRight = savedPair ? loaded.find((image) => image.id === savedPair[1]) : undefined
      updateReviewSession({ mode: 'arena', groupKey: currentGroupKey, granularity })
      if (restoredLeft && restoredRight && restoredLeft.id !== restoredRight.id) {
        setLeft(restoredLeft)
        setRight(restoredRight)
        recent.current.add(pairKey(restoredLeft.id, restoredRight.id))
        startedAtRef.current = new Date().toISOString()
      } else {
        selectPair(loaded, nextScores)
      }
      setStatus('ready')
    } catch (error) {
      if (request !== loadRequestRef.current) return
      setLoadError(errorMessage(error))
      setStatus('error')
    }
  }, [currentGroupKey, granularity, selectPair, updateReviewSession])

  useEffect(() => {
    void load()
    return () => {
      loadRequestRef.current += 1
      if (transitionTimerRef.current) clearTimeout(transitionTimerRef.current)
    }
  }, [load])

  async function vote(winnerIsLeft: boolean) {
    if (!left || !right || !currentGroupKey || busy) return
    const request = ++actionRequestRef.current
    const pairLeft = left
    const pairRight = right
    const startedMs = Date.now()
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    try {
      const [newLeft, newRight] = await arenaVote(
        currentGroupKey,
        pairLeft.id,
        pairRight.id,
        winnerIsLeft,
        {
          sessionId: getTelemetrySessionId(),
          startedAt: startedAtRef.current,
          contextSignature: currentGroupKey,
        },
      )
      if (request !== actionRequestRef.current) return
      const nextScores = { ...scores, [pairLeft.id]: newLeft, [pairRight.id]: newRight }
      setScores(nextScores)
      trackAction('arena_vote_commit', {
        side: winnerIsLeft ? 'left' : 'right',
        duration_ms: Date.now() - startedMs,
        left_id: pairLeft.id,
        right_id: pairRight.id,
      })
      setFly(winnerIsLeft ? 'right' : 'left')
      transitionTimerRef.current = setTimeout(() => {
        if (request !== actionRequestRef.current) return
        setFly(null)
        selectPair(images, nextScores)
        actionInFlightRef.current = false
        setBusy(false)
        if (reloadAfterActionRef.current) {
          reloadAfterActionRef.current = false
          void load()
        }
      }, 220)
    } catch (error) {
      if (request !== actionRequestRef.current) return
      setFly(null)
      actionInFlightRef.current = false
      setBusy(false)
      setRetryAction({ kind: 'vote', winnerIsLeft })
      setActionError(errorMessage(error))
    }
  }

  async function hideCard(image: ImageRow) {
    if (busy || !left || !right || !currentGroupKey) return
    const request = ++actionRequestRef.current
    const startedMs = Date.now()
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    setLeftPopoverOpen(false)
    setRightPopoverOpen(false)
    // Drop focus: a card keeps an Enter/Space vote handler, and after a
    // Shift+←/→ hide the next Enter press must NOT vote by accident.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur()
    const survivor = image.id === left.id ? right : left
    try {
      // Hiding credits the survivor exactly like an arena winner: one
      // atomic action snapshots both images so undo restores the pair.
      const result = await arenaHide(currentGroupKey, survivor.id, image.id, {
        sessionId: getTelemetrySessionId(),
        startedAt: startedAtRef.current,
        contextSignature: currentGroupKey,
      })
      if (request !== actionRequestRef.current) return
      const nextImages = images.filter((item) => item.id !== image.id)
      const nextScores = {
        ...scores,
        [image.id]: result.victim_score,
        [survivor.id]: result.survivor_score,
      }
      updateReviewSession({
        arenaLastHideActionId: result.action_id,
        arenaLastHiddenImageId: image.id,
        arenaLastHidePair: [left.id, right.id],
      })
      setImages(nextImages)
      setScores(nextScores)
      trackAction('hide', {
        hidden: true,
        mode: 'arena',
        duration_ms: Date.now() - startedMs,
        image_id: image.id,
      })
      selectPair(nextImages, nextScores)
      actionInFlightRef.current = false
      setBusy(false)
      if (reloadAfterActionRef.current) {
        reloadAfterActionRef.current = false
        void load()
      }
    } catch (error) {
      if (request !== actionRequestRef.current) return
      actionInFlightRef.current = false
      setBusy(false)
      setRetryAction({ kind: 'hide', image })
      setActionError(errorMessage(error))
    }
  }

  async function undoLastHide() {
    const session = useStore.getState().reviewSession
    const actionId = session.arenaLastHideActionId
    const hiddenImageId = session.arenaLastHiddenImageId
    const restorePair = session.arenaLastHidePair
    if (!actionId || hiddenImageId == null || busy || currentGroupKey == null) return
    const request = ++actionRequestRef.current
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    try {
      await undoReviewAction(actionId, getTelemetrySessionId())
      if (request !== actionRequestRef.current) return
      const nextImages = await listGroupImages(currentGroupKey, granularity)
      if (request !== actionRequestRef.current) return
      const nextScores: Record<number, number> = {}
      nextImages.forEach((image) => {
        if (image.score != null) nextScores[image.id] = image.score
      })
      setImages(nextImages)
      setScores(nextScores)
      updateReviewSession({
        arenaLastHideActionId: null,
        arenaLastHiddenImageId: null,
        arenaLastHidePair: null,
      })
      trackAction('hide', { hidden: false, mode: 'arena', image_id: hiddenImageId })
      if (restorePair) {
        // Undo must land back on the EXACT pair that was on screen when the
        // hide happened — not a random next pair — so the user can redo.
        const restoredLeft = nextImages.find((image) => image.id === restorePair[0])
        const restoredRight = nextImages.find((image) => image.id === restorePair[1])
        if (restoredLeft && restoredRight && restoredLeft.id !== restoredRight.id) {
          setLeft(restoredLeft)
          setRight(restoredRight)
          recent.current.add(pairKey(restoredLeft.id, restoredRight.id))
          startedAtRef.current = new Date().toISOString()
          updateReviewSession({ arenaPair: [restoredLeft.id, restoredRight.id] })
        } else {
          selectPair(nextImages, nextScores)
        }
      } else {
        selectPair(nextImages, nextScores)
      }
      actionInFlightRef.current = false
      setBusy(false)
      if (reloadAfterActionRef.current) {
        reloadAfterActionRef.current = false
        void load()
      }
    } catch (error) {
      if (request !== actionRequestRef.current) return
      actionInFlightRef.current = false
      setBusy(false)
      setRetryAction({ kind: 'undo' })
      setActionError(errorMessage(error))
    }
  }

  function retry() {
    if (!retryAction) return
    if (retryAction.kind === 'vote') void vote(retryAction.winnerIsLeft)
    else if (retryAction.kind === 'hide') void hideCard(retryAction.image)
    else void undoLastHide()
  }

  useEffect(() => {
    const clearPendingHide = () => setPendingHide(false)
    const onVisibility = () => {
      if (document.visibilityState !== 'visible') clearPendingHide()
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (lightbox || leftPopoverOpen || rightPopoverOpen) return
      if (isEditableTarget(event.target)) return
      if (matchesBinding(keybindings.arenaArmHide, event)) {
        event.preventDefault()
        if (!busy) setPendingHide(true)
      } else if (matchesBinding(keybindings.arenaHideLeft, event)) {
        event.preventDefault()
        if (left && !busy) void hideCard(left)
      } else if (matchesBinding(keybindings.arenaHideRight, event)) {
        event.preventDefault()
        if (right && !busy) void hideCard(right)
      } else if (matchesBinding(keybindings.arenaVoteLeft, event)) {
        event.preventDefault()
        void vote(true)
      } else if (matchesBinding(keybindings.arenaVoteRight, event)) {
        event.preventDefault()
        void vote(false)
      } else if (matchesBinding(keybindings.arenaSkip, event)) {
        event.preventDefault()
        if (!busy) selectPair(images, scores)
      } else if (matchesBinding(keybindings.arenaUndoHide, event)) {
        event.preventDefault()
        void undoLastHide()
      }
    }
    const onKeyUp = (event: KeyboardEvent) => {
      if (event.key === 'Shift') clearPendingHide()
    }
    window.addEventListener('keydown', onKeyDown)
    window.addEventListener('keyup', onKeyUp)
    window.addEventListener('blur', clearPendingHide)
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      window.removeEventListener('keyup', onKeyUp)
      window.removeEventListener('blur', clearPendingHide)
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [busy, images, keybindings, left, leftPopoverOpen, lightbox, right, rightPopoverOpen, scores, selectPair])

  const leftScore = left ? scores[left.id] ?? 50 : 0
  const rightScore = right ? scores[right.id] ?? 50 : 0

  if (currentGroupKey == null) {
    return <StatePanel message="未选择分组。" actionLabel="返回分组" onAction={() => setView('groups')} />
  }
  if (status === 'loading') return <StatePanel message="正在加载擂台图片…" />
  if (status === 'error') {
    return <StatePanel message={`加载失败：${loadError}`} actionLabel="重试" onAction={() => void load()} />
  }
  if (!left || !right) {
    return <StatePanel message="本组不足两张可评分图片，无法擂台。" actionLabel="返回滑卡" onAction={() => setView('swipe')} />
  }

  return (
    <div className="arena-view">
      <div className="swipe-topbar">
        <button type="button" onClick={() => setView('swipe')}>← 返回滑卡</button>
        <span className="counter">擂台模式</span>
        <button type="button" onClick={() => setView('folder')}>文件夹视角</button>
        <button type="button" onClick={() => selectPair(images, scores)} disabled={busy}>下一对</button>
      </div>
      {actionError && (
        <div className="action-error" role="alert">
          <span>{actionError}</span>
          <button type="button" disabled={busy} onClick={retry}>重试</button>
        </div>
      )}
      {pendingHide && (
        <div className="arena-pending-hint">
          待屏蔽：按住 Shift，{bindingDisplay(keybindings.arenaHideLeft)} 屏蔽左卡 ·{' '}
          {bindingDisplay(keybindings.arenaHideRight)} 屏蔽右卡 · 松开 Shift 取消
        </div>
      )}
      {reviewSession.arenaLastHideActionId && (
        <div className="arena-undo-bar" role="status">
          <span>已屏蔽一张图片（幸存方已按胜者计分）</span>
          <button type="button" disabled={busy} onClick={() => void undoLastHide()}>
            {bindingDisplay(keybindings.arenaUndoHide)} 撤销并回到上一对
          </button>
        </div>
      )}
      <div className="arena-stage">
        <div
          className={`arena-card ${pendingHide ? 'arena-pending' : ''} ${fly === 'left' ? 'fly-left' : ''} ${fly === 'right' ? 'fly-right' : ''}`}
          role="button"
          tabIndex={0}
          aria-label={pendingHide ? '屏蔽左侧图片' : '选择左侧图片胜出'}
          onClick={() => pendingHide ? void hideCard(left) : void vote(true)}
          onKeyDown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault()
              pendingHide ? void hideCard(left) : void vote(true)
            }
          }}
        >
          <img src={assetUrl(left.abs_path)} alt={left.filename} draggable={false} onClick={(event) => { event.stopPropagation(); setLightbox(left.abs_path) }} />
          <Popover open={leftPopoverOpen} onOpenChange={setLeftPopoverOpen} trigger={<button type="button" className="more-btn">更多 ▾</button>}>
            <ImageMetaPopover img={left} comparePrompt={bothOpen ? right.prompt_pos : undefined} diffColor="left" onHide={hideCard} />
          </Popover>
          <div className="arena-score">{leftScore.toFixed(1)}</div>
          <div className="arena-label">{pendingHide ? '← 屏蔽此卡' : '点击/← 胜出'}</div>
        </div>
        <div className="arena-vs">VS</div>
        <div
          className={`arena-card ${pendingHide ? 'arena-pending' : ''} ${fly === 'right' ? 'fly-left' : ''} ${fly === 'left' ? 'fly-right' : ''}`}
          role="button"
          tabIndex={0}
          aria-label={pendingHide ? '屏蔽右侧图片' : '选择右侧图片胜出'}
          onClick={() => pendingHide ? void hideCard(right) : void vote(false)}
          onKeyDown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault()
              pendingHide ? void hideCard(right) : void vote(false)
            }
          }}
        >
          <img src={assetUrl(right.abs_path)} alt={right.filename} draggable={false} onClick={(event) => { event.stopPropagation(); setLightbox(right.abs_path) }} />
          <Popover open={rightPopoverOpen} onOpenChange={setRightPopoverOpen} trigger={<button type="button" className="more-btn">更多 ▾</button>}>
            <ImageMetaPopover img={right} comparePrompt={bothOpen ? left.prompt_pos : undefined} diffColor="right" onHide={hideCard} />
          </Popover>
          <div className="arena-score">{rightScore.toFixed(1)}</div>
          <div className="arena-label">{pendingHide ? '→ 屏蔽此卡' : '点击/→ 胜出'}</div>
        </div>
      </div>
      <p className="muted hint">
        {bindingDisplay(keybindings.arenaVoteLeft)} / {bindingDisplay(keybindings.arenaVoteRight)} 选胜方 ·{' '}
        {bindingDisplay(keybindings.arenaSkip)} 跳过 · 按住 {bindingDisplay(keybindings.arenaArmHide)} 后按{' '}
        {bindingDisplay(keybindings.arenaHideLeft)} / {bindingDisplay(keybindings.arenaHideRight)} 屏蔽对应卡 ·{' '}
        {bindingDisplay(keybindings.arenaUndoHide)} 撤销最近屏蔽（回到上一对，双方分数一并还原）· 屏蔽后幸存方按胜者计分 · 单击图片放大 · 两侧"更多"可屏蔽并查看 Prompt 差异高亮
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
