import { useCallback, useEffect, useRef, useState } from 'react'
import { arenaHide, arenaVote, assetUrl, errorMessage, listGroupImages, trashImage, undoReviewAction } from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'
import type { ArenaHistoryEntry } from '../store'
import { bindingDisplay, matchesBinding } from '../keymap'
import { getTelemetrySessionId, trackAction } from '../telemetry'
import { Lightbox } from './Lightbox'
import { Popover } from './Popover'
import { ImageMetaPopover } from './ImageMetaPopover'
import { Thumb } from './Thumb'
import { pairKey, pickArenaPair } from '../lib/arenaPair'

const RECENT_BASE_LIMIT = 20
/** 操作历史的长度上限 —— 快照只在本进入擂台期间有效，不该无限长。 */
const HISTORY_LIMIT = 50

function isEditableTarget(target: EventTarget | null) {
  return target instanceof HTMLElement && (
    target.matches('input, textarea, select, [contenteditable="true"]') ||
    target.closest('[contenteditable="true"]') != null
  )
}

type RetryAction =
  | { kind: 'vote'; winnerIsLeft: boolean }
  | { kind: 'hide'; image: ImageRow }
  | { kind: 'back' }

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
  const [lightbox, setLightbox] = useState<ImageRow | null>(null)
  const [fly, setFly] = useState<'left' | 'right' | null>(null)
  const [leftPopoverOpen, setLeftPopoverOpen] = useState(false)
  const [rightPopoverOpen, setRightPopoverOpen] = useState(false)
  const [pendingHide, setPendingHide] = useState(false)
  const [notice, setNotice] = useState('')
  const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const recent = useRef<Set<string>>(new Set())
  const lastShownRound = useRef<Record<number, number>>({})
  const roundCounter = useRef(0)
  const loadRequestRef = useRef(0)
  const actionRequestRef = useRef(0)
  const actionInFlightRef = useRef(false)
  const reloadAfterActionRef = useRef(false)
  const transitionTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const startedAtRef = useRef(new Date().toISOString())
  const bothOpen = leftPopoverOpen && rightPopoverOpen

  const markShown = useCallback((l: ImageRow, r: ImageRow) => {
    roundCounter.current += 1
    lastShownRound.current[l.id] = roundCounter.current
    lastShownRound.current[r.id] = roundCounter.current
    recent.current.add(pairKey(l.id, r.id))
    // Strict LRU: drop the OLDEST insertion so capacity N pairs of pairs
    // covers recent history, instead of blanket-killing half the memory.
    const capacity = Math.max(RECENT_BASE_LIMIT, 2 * Math.max(1, recent.current.size))
    const keys = Array.from(recent.current)
    let overflow = keys.length - capacity
    while (overflow > 0) {
      const oldest = keys.shift()
      if (oldest === undefined) break
      recent.current.delete(oldest)
      overflow -= 1
    }
  }, [])

  const selectPair = useCallback((nextImages: ImageRow[], nextScores: Record<number, number>) => {
    const pick = pickArenaPair({
      images: nextImages,
      scores: nextScores,
      recentPairs: recent.current,
      lastShownRound: lastShownRound.current,
      round: roundCounter.current,
    })
    if (!pick) {
      setLeft(null)
      setRight(null)
      updateReviewSession({ arenaPair: null })
      return
    }
    const [nextLeft, nextRight] = pick
    markShown(nextLeft, nextRight)
    setLeft(nextLeft)
    setRight(nextRight)
    startedAtRef.current = new Date().toISOString()
    updateReviewSession({ arenaPair: [nextLeft.id, nextRight.id] })
  }, [updateReviewSession, markShown])

  /** 记一步操作 —— Backspace 的依据。栈，不是「最后一次屏蔽」那一个槽。 */
  const pushHistory = useCallback((entry: ArenaHistoryEntry) => {
    const session = useStore.getState().reviewSession
    const next = [...(session.arenaHistory ?? []), entry].slice(-HISTORY_LIMIT)
    updateReviewSession({ arenaHistory: next })
  }, [updateReviewSession])

  /** 一次性反馈行（删除等动作的结果）—— 没有确认框，就得有回执。 */
  const showNotice = useCallback((text: string) => {
    setNotice(text)
    if (noticeTimer.current) clearTimeout(noticeTimer.current)
    noticeTimer.current = setTimeout(() => setNotice(''), 4000)
  }, [])

  useEffect(() => () => {
    if (noticeTimer.current) clearTimeout(noticeTimer.current)
  }, [])

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
        markShown(restoredLeft, restoredRight)
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
  }, [currentGroupKey, granularity, markShown, selectPair, updateReviewSession])

  useEffect(() => {
    void load()
    return () => {
      loadRequestRef.current += 1
      if (transitionTimerRef.current) clearTimeout(transitionTimerRef.current)
    }
  }, [load])

  // 操作历史只在**本次进入擂台模式**期间有效：下次进入擂台即清空。
  // 快照因此不会在应用状态里越积越长（也不进数据库结构）。
  useEffect(() => {
    updateReviewSession({ arenaHistory: [] })
  }, [updateReviewSession])

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
      const result = await arenaVote(
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
      const nextScores = { ...scores, [pairLeft.id]: result.left_score, [pairRight.id]: result.right_score }
      setScores(nextScores)
      // 这一票记进历史：Backspace 能回到这一组，并把双方分数还原。
      pushHistory({
        kind: 'vote',
        pair: [pairLeft.id, pairRight.id],
        actionId: result.action_id,
        winnerIsLeft,
      })
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
      // 屏蔽同样记进历史：Backspace 回到这一组时撤销屏蔽（图放回队列）。
      pushHistory({
        kind: 'hide',
        pair: [left.id, right.id],
        actionId: result.action_id,
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

  /**
   * Backspace = 回到上一组。
   *
   * 「上一组」是**用户真正操作过的那一组**（投票 / 屏蔽 / 跳过），而不是
   * 「上一次屏蔽的那一组」—— 后者会把用户甩到几轮之前（本轮修掉的毛病）。
   * 回到时按动作类型回滚：投票回滚双方分数；屏蔽撤销屏蔽并把图放回队列。
   */
  const goBack = useCallback(async () => {
    const session = useStore.getState().reviewSession
    const stack = session.arenaHistory ?? []
    const entry = stack[stack.length - 1]
    if (!entry || busy || currentGroupKey == null) return
    updateReviewSession({ arenaHistory: stack.slice(0, -1) })
    const request = ++actionRequestRef.current
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    try {
      if (entry.actionId) {
        // 投票与屏蔽都由后端按快照还原分数；屏蔽还会把图放回队列。
        await undoReviewAction(entry.actionId, getTelemetrySessionId())
      }
      const nextImages = await listGroupImages(currentGroupKey, granularity)
      if (request !== actionRequestRef.current) return
      const nextScores: Record<number, number> = {}
      nextImages.forEach((image) => {
        if (image.score != null) nextScores[image.id] = image.score
      })
      setImages(nextImages)
      setScores(nextScores)
      const restoredLeft = nextImages.find((image) => image.id === entry.pair[0])
      const restoredRight = nextImages.find((image) => image.id === entry.pair[1])
      if (restoredLeft && restoredRight && restoredLeft.id !== restoredRight.id) {
        setLeft(restoredLeft)
        setRight(restoredRight)
        markShown(restoredLeft, restoredRight)
        startedAtRef.current = new Date().toISOString()
        updateReviewSession({ arenaPair: [restoredLeft.id, restoredRight.id] })
      } else {
        selectPair(nextImages, nextScores)
      }
      trackAction('arena_back', { kind: entry.kind })
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
      setRetryAction({ kind: 'back' })
      setActionError(errorMessage(error))
      // 失败就把这一条**放回**栈里 —— 否则用户的「上一步」凭空消失，
      // 而「重试」会退回到更早的那一步（错的）。
      const current = useStore.getState().reviewSession
      updateReviewSession({
        arenaHistory: [...(current.arenaHistory ?? []), entry].slice(-HISTORY_LIMIT),
      })
    }
  }, [busy, currentGroupKey, granularity, load, markShown, selectPair, updateReviewSession])

  /** 跳过这一对 —— 也算一步操作，Backspace 能回到它（不改分）。 */
  const skipPair = useCallback(() => {
    if (!left || !right) return
    pushHistory({ kind: 'skip', pair: [left.id, right.id], actionId: null })
    selectPair(images, scores)
  }, [images, left, pushHistory, right, scores, selectPair])

  /**
   * 查看器里按 Delete：图片进系统回收站（可从桌面恢复）。
   * 不做二次确认 —— 用户按的就是「删除」，回收站本身就是后悔药。
   */
  const deleteImage = useCallback(async (image: ImageRow) => {
    if (busy) return
    const request = ++actionRequestRef.current
    actionInFlightRef.current = true
    setBusy(true)
    setActionError('')
    setRetryAction(null)
    try {
      await trashImage(image.id)
      if (request !== actionRequestRef.current) return
      const nextImages = images.filter((item) => item.id !== image.id)
      const nextScores = { ...scores }
      delete nextScores[image.id]
      setImages(nextImages)
      setScores(nextScores)
      // 历史里涉及这张图的记录一并作废：那一对已经不存在了。
      const session = useStore.getState().reviewSession
      updateReviewSession({
        arenaHistory: (session.arenaHistory ?? []).filter(
          (item) => item.pair[0] !== image.id && item.pair[1] !== image.id,
        ),
      })
      setLightbox(null)
      showNotice(`已送入回收站：${image.filename}`)
      trackAction('image_delete', { mode: 'arena', image_id: image.id })
      if (!left || !right || left.id === image.id || right.id === image.id) {
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
      setActionError(errorMessage(error))
    }
  }, [busy, images, left, load, right, scores, selectPair, showNotice, updateReviewSession])

  function retry() {
    if (!retryAction) return
    if (retryAction.kind === 'vote') void vote(retryAction.winnerIsLeft)
    else if (retryAction.kind === 'hide') void hideCard(retryAction.image)
    else void goBack()
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
        if (!busy) skipPair()
      } else if (matchesBinding(keybindings.arenaUndoHide, event)) {
        event.preventDefault()
        void goBack()
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
  }, [busy, goBack, images, keybindings, left, leftPopoverOpen, lightbox, right, rightPopoverOpen, scores, skipPair])

  const leftScore = left ? scores[left.id] ?? 50 : 0
  const rightScore = right ? scores[right.id] ?? 50 : 0
  // 防御：持久化里的 reviewSession 可能是**旧形状**（没有 arenaHistory）。
  const history = reviewSession.arenaHistory ?? []
  const lastHistory = history[history.length - 1] ?? null

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
        <button type="button" onClick={skipPair} disabled={busy}>下一对</button>
      </div>
      {actionError && (
        <div className="action-error" role="alert">
          <span>{actionError}</span>
          <button type="button" disabled={busy} onClick={retry}>重试</button>
        </div>
      )}
      {notice && <div className="notice-line" role="status">{notice}</div>}
      {pendingHide && (
        <div className="arena-pending-hint">
          待屏蔽：按住 Shift，{bindingDisplay(keybindings.arenaHideLeft)} 屏蔽左卡 ·{' '}
          {bindingDisplay(keybindings.arenaHideRight)} 屏蔽右卡 · 松开 Shift 取消
        </div>
      )}
      {lastHistory && (
        <div className="arena-undo-bar" role="status">
          <span>{backHint(lastHistory)}</span>
          <button type="button" disabled={busy} onClick={() => void goBack()}>
            {bindingDisplay(keybindings.arenaUndoHide)} 回到上一组
          </button>
        </div>
      )}
      <div className="arena-stage">
        <div
          className={`arena-card ${pendingHide ? 'arena-pending' : ''} ${fly === 'left' ? 'fly-left' : ''} ${fly === 'right' ? 'fly-right' : ''}`}
          role="group"
          aria-label={pendingHide ? '屏蔽左侧图片' : '左侧图片'}
        >
          <Thumb
            src={assetUrl(left.abs_path)}
            alt={left.filename}
            fileMissing={left.file_missing}
            draggable={false}
            onClick={() => setLightbox(left)}
          />
          <Popover open={leftPopoverOpen} onOpenChange={setLeftPopoverOpen} trigger={<button type="button" className="more-btn">更多 ▾</button>}>
            <ImageMetaPopover img={left} comparePrompt={bothOpen ? right.prompt_pos : undefined} diffColor="left" onHide={hideCard} />
          </Popover>
          <div className="arena-score">{leftScore.toFixed(1)}</div>
          <button
            type="button"
            className="arena-vote-btn"
            disabled={busy}
            onClick={() => (pendingHide ? void hideCard(left) : void vote(true))}
            aria-label={pendingHide ? '屏蔽这张图片' : '选这张图片胜出'}
          >
            {pendingHide ? '屏蔽此卡（←）' : '选它胜出（←）'}
          </button>
        </div>
        <div className="arena-vs">VS</div>
        <div
          className={`arena-card ${pendingHide ? 'arena-pending' : ''} ${fly === 'right' ? 'fly-left' : ''} ${fly === 'left' ? 'fly-right' : ''}`}
          role="group"
          aria-label={pendingHide ? '屏蔽右侧图片' : '右侧图片'}
        >
          <Thumb
            src={assetUrl(right.abs_path)}
            alt={right.filename}
            fileMissing={right.file_missing}
            draggable={false}
            onClick={() => setLightbox(right)}
          />
          <Popover open={rightPopoverOpen} onOpenChange={setRightPopoverOpen} trigger={<button type="button" className="more-btn">更多 ▾</button>}>
            <ImageMetaPopover img={right} comparePrompt={bothOpen ? left.prompt_pos : undefined} diffColor="right" onHide={hideCard} />
          </Popover>
          <div className="arena-score">{rightScore.toFixed(1)}</div>
          <button
            type="button"
            className="arena-vote-btn"
            disabled={busy}
            onClick={() => (pendingHide ? void hideCard(right) : void vote(false))}
            aria-label={pendingHide ? '屏蔽这张图片' : '选这张图片胜出'}
          >
            {pendingHide ? '屏蔽此卡（→）' : '选它胜出（→）'}
          </button>
        </div>
      </div>
      <p className="muted hint arena-hint-text">
        <b>{bindingDisplay(keybindings.arenaVoteLeft)} / {bindingDisplay(keybindings.arenaVoteRight)}</b> 选胜方（也可点卡片下方按钮）·{' '}
        <b>{bindingDisplay(keybindings.arenaSkip)}</b> 跳过 · 按住 <b>{bindingDisplay(keybindings.arenaArmHide)}</b> 后按{' '}
        <b>{bindingDisplay(keybindings.arenaHideLeft)} / {bindingDisplay(keybindings.arenaHideRight)}</b> 屏蔽对应卡 ·{' '}
        <b>{bindingDisplay(keybindings.arenaUndoHide)}</b> 回到上一组（分数一并回滚）· 屏蔽后幸存方按胜者计分 ·{' '}
        点图片放大查看 · 两侧"更多"可屏蔽并查看 Prompt 差异
      </p>
      {lightbox && (
        <Lightbox
          src={assetUrl(lightbox.abs_path)}
          target={lightbox}
          onDelete={(image) => void deleteImage(image)}
          onClose={() => setLightbox(null)}
        />
      )}
    </div>
  )
}

/** 撤销条上的一句话：说清楚「按下去会发生什么」。 */
function backHint(entry: ArenaHistoryEntry): string {
  if (entry.kind === 'vote') return '上一组：已投票（回到它，双方分数一并回滚）'
  if (entry.kind === 'hide') return '上一组：已屏蔽一张（回到它，撤销屏蔽）'
  return '上一组：已跳过（回到它，分数不变）'
}

function StatePanel({ message, actionLabel, onAction }: { message: string; actionLabel?: string; onAction?: () => void }) {
  return (
    <div className="panel state-panel" role="status">
      <p className="muted">{message}</p>
      {actionLabel && onAction && <button type="button" onClick={onAction}>{actionLabel}</button>}
    </div>
  )
}
