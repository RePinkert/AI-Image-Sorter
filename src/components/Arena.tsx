import { useEffect, useRef, useState } from 'react'
import { arenaVote, assetUrl, listGroupImages, toggleHidden as toggleHiddenApi } from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'
import { bindingDisplay, matchesBinding } from '../keymap'
import { Lightbox } from './Lightbox'
import { Popover } from './Popover'
import { ImageMetaPopover } from './ImageMetaPopover'

// Bradley-Terry 分差阈值 —— 与 scoring.rs::ARENA_THRESHOLD 一致。
const ARENA_THRESHOLD = 5
// 内存 LRU：保留最近 N 对已比对组合，避免短期重复抽到同一对。
const RECENT_LIMIT = 20

function pairKey(a: number, b: number) {
  return a < b ? `${a}:${b}` : `${b}:${a}`
}

export function Arena() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const keybindings = useStore((s) => s.keybindings)
  const [images, setImages] = useState<ImageRow[]>([])
  const [left, setLeft] = useState<ImageRow | null>(null)
  const [right, setRight] = useState<ImageRow | null>(null)
  const [scores, setScores] = useState<Record<number, number>>({})
  const [busy, setBusy] = useState(false)
  const [lightbox, setLightbox] = useState<string | null>(null)
  // In-memory短期去重：每次抽对前先剔除最近 RECENT_LIMIT 对。
  const recent = useRef<Set<string>>(new Set())
  // 决出胜负后的飞出动画方向；用于给落败卡片一个明确的视觉收束。
  const [fly, setFly] = useState<'left' | 'right' | null>(null)
  const [leftPopoverOpen, setLeftPopoverOpen] = useState(false)
  const [rightPopoverOpen, setRightPopoverOpen] = useState(false)
  const bothOpen = leftPopoverOpen && rightPopoverOpen
  // 待屏蔽悬停态：按住 Shift 触发提示（松开 Shift 取消），Shift+←/→ 屏蔽对应卡。
  const [pendingHide, setPendingHide] = useState(false)
  // 最近一次屏蔽的卡（用于 Backspace 撤销，分数保持 0）。
  const lastHidden = useRef<ImageRow | null>(null)

  useEffect(() => {
    if (currentGroupKey == null) return
    listGroupImages(currentGroupKey, granularity).then((imgs) => {
      setImages(imgs)
      const sc: Record<number, number> = {}
      imgs.forEach((i) => {
        if (i.score != null) sc[i.id] = i.score
      })
      setScores(sc)
      pickPair(imgs, sc)
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentGroupKey, granularity])

  // 随机抽对：从评分差 <ARENA_THRESHOLD 的候选集中随机选一对，避
  // 开 recent LRU；候选不足则回退到全集合随机。这样每一对被决出胜
  // 负后会立即进入洗牌流程，而不是像旧的贪心首对那样反复留存。
  function pickPair(imgs: ImageRow[], sc: Record<number, number>) {
    const candidates: [ImageRow, ImageRow][] = []
    for (let i = 0; i < imgs.length; i++) {
      for (let j = i + 1; j < imgs.length; j++) {
        const a = imgs[i]
        const b = imgs[j]
        const sa = sc[a.id] ?? 50
        const sb = sc[b.id] ?? 50
        if (Math.abs(sa - sb) < ARENA_THRESHOLD) candidates.push([a, b])
      }
    }
    const pool =
      candidates.length > 0
        ? candidates
        : imgs.length >= 2
        ? pairAll(imgs)
        : []
    // Filter out recently-seen pairs.
    const fresh = pool.filter(([a, b]) => !recent.current.has(pairKey(a.id, b.id)))
    const choice = fresh.length > 0 ? fresh : pool
    if (choice.length === 0) {
      setLeft(null)
      setRight(null)
      return
    }
    const [a, b] = choice[Math.floor(Math.random() * choice.length)]
    recent.current.add(pairKey(a.id, b.id))
    if (recent.current.size > RECENT_LIMIT) {
      // Drop oldest by clearing ~half (JS Sets preserve insertion order).
      const arr = Array.from(recent.current)
      recent.current = new Set(arr.slice(Math.floor(arr.length / 2)))
    }
    setLeft(a)
    setRight(b)
  }

  function pairAll(imgs: ImageRow[]): [ImageRow, ImageRow][] {
    const out: [ImageRow, ImageRow][] = []
    for (let i = 0; i < imgs.length; i++) {
      for (let j = i + 1; j < imgs.length; j++) out.push([imgs[i], imgs[j]])
    }
    return out
  }

  async function vote(winnerIsLeft: boolean) {
    if (!left || !right || !currentGroupKey || busy) return
    setBusy(true)
    setFly(winnerIsLeft ? 'right' : 'left')
    try {
      const [nl, nr] = await arenaVote(currentGroupKey, left.id, right.id, winnerIsLeft)
      setScores((s) => ({ ...s, [left.id]: nl, [right.id]: nr }))
      // 给飞出动画一帧时间再下一对。
      setTimeout(() => {
        setFly(null)
        pickPair(images, { ...scores, [left.id]: nl, [right.id]: nr })
        setBusy(false)
      }, 220)
    } catch {
      setFly(null)
      setBusy(false)
    }
  }

  // 屏蔽一张卡：赋 0 分（后端）、移出候选集、立即重抽下一对。
  // 待屏蔽提示由 Shift 的 keyup 复位（按住 Shift 可连续屏蔽多张），这里不清除。
  async function hideCard(img: ImageRow) {
    if (busy) return
    setBusy(true)
    setLeftPopoverOpen(false)
    setRightPopoverOpen(false)
    try {
      await toggleHiddenApi(img.id, true)
      const nextImages = images.filter((x) => x.id !== img.id)
      const nextScores = { ...scores, [img.id]: 0 }
      lastHidden.current = img
      setImages(nextImages)
      setScores(nextScores)
      pickPair(nextImages, nextScores)
    } catch {
      // 保留当前对，等待重试
    } finally {
      setBusy(false)
    }
  }

  // 撤销最近一次屏蔽：取消 hidden（分数保持 0），放回候选池。
  async function undoLastHide() {
    const img = lastHidden.current
    if (!img || busy) return
    setBusy(true)
    try {
      await toggleHiddenApi(img.id, false)
      lastHidden.current = null
      const nextImages = images.some((x) => x.id === img.id) ? images : [...images, img]
      setImages(nextImages)
      pickPair(nextImages, scores)
    } catch {
      // ignore
    } finally {
      setBusy(false)
    }
  }

  // 键盘：左 → 左胜 / 右 → 右胜 / 空格 → 下一对。
  // 屏蔽为组合键：按住 Shift 触发待屏蔽提示（不松开即可看到），
  // Shift+← / Shift+→ 直接屏蔽对应卡；松开 Shift 取消提示。
  // Backspace 撤销最近一次屏蔽。
  // 弹窗或灯箱打开时不响应擂台键，避免复制 Prompt 时误触发投票/屏蔽。
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (lightbox) return
      if (leftPopoverOpen || rightPopoverOpen) return
      if (matchesBinding(keybindings.arenaArmHide, e)) {
        e.preventDefault()
        if (!busy) setPendingHide(true)
      } else if (matchesBinding(keybindings.arenaHideLeft, e)) {
        e.preventDefault()
        if (left && !busy) hideCard(left)
      } else if (matchesBinding(keybindings.arenaHideRight, e)) {
        e.preventDefault()
        if (right && !busy) hideCard(right)
      } else if (matchesBinding(keybindings.arenaVoteLeft, e)) {
        e.preventDefault()
        vote(true)
      } else if (matchesBinding(keybindings.arenaVoteRight, e)) {
        e.preventDefault()
        vote(false)
      } else if (matchesBinding(keybindings.arenaSkip, e)) {
        e.preventDefault()
        if (!busy) pickPair(images, scores)
      } else if (matchesBinding(keybindings.arenaUndoHide, e)) {
        e.preventDefault()
        undoLastHide()
      }
    }
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === 'Shift') setPendingHide(false)
    }
    window.addEventListener('keydown', onKeyDown)
    window.addEventListener('keyup', onKeyUp)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      window.removeEventListener('keyup', onKeyUp)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [left, right, busy, images, scores, lightbox, leftPopoverOpen, rightPopoverOpen, keybindings])

  const leftScore = left ? scores[left.id] ?? 50 : 0
  const rightScore = right ? scores[right.id] ?? 50 : 0

  if (!left || !right) {
    return (
      <div className="panel">
        <p className="muted">本组不足两张图片，无法擂台。</p>
        <button onClick={() => setView('swipe')}>返回</button>
      </div>
    )
  }

  return (
    <div className="arena-view">
      <div className="swipe-topbar">
        <button onClick={() => setView('swipe')}>← 返回滑卡</button>
        <span className="counter">擂台模式</span>
        <button onClick={() => setView('folder')}>文件夹视角</button>
        <button onClick={() => pickPair(images, scores)} disabled={busy}>
          下一对
        </button>
      </div>
      {pendingHide && (
        <div className="arena-pending-hint">
          待屏蔽：按住 Shift，{bindingDisplay(keybindings.arenaHideLeft)} 屏蔽左卡 ·{' '}
          {bindingDisplay(keybindings.arenaHideRight)} 屏蔽右卡 · 松开 Shift 取消
        </div>
      )}
      <div className="arena-stage">
        <div
          className={`arena-card ${pendingHide ? 'arena-pending' : ''} ${fly === 'left' ? 'fly-left' : ''} ${fly === 'right' ? 'fly-right' : ''}`}
          onClick={() => (pendingHide ? hideCard(left) : vote(true))}
        >
          <img
            src={assetUrl(left.abs_path)}
            alt={left.filename}
            draggable={false}
            onClick={(e) => {
              e.stopPropagation()
              setLightbox(left.abs_path)
            }}
          />
          <Popover
            open={leftPopoverOpen}
            onOpenChange={setLeftPopoverOpen}
            trigger={<span className="more-btn">更多 ▾</span>}
          >
            <ImageMetaPopover
              img={left}
              comparePrompt={bothOpen ? right.prompt_pos : undefined}
              diffColor="left"
              onHide={(img) => hideCard(img)}
            />
          </Popover>
          <div className="arena-score">{leftScore.toFixed(1)}</div>
          <div className="arena-label">{pendingHide ? '← 屏蔽此卡' : '点击/← 胜出'}</div>
        </div>
        <div className="arena-vs">VS</div>
        <div
          className={`arena-card ${pendingHide ? 'arena-pending' : ''} ${fly === 'right' ? 'fly-left' : ''} ${fly === 'left' ? 'fly-right' : ''}`}
          onClick={() => (pendingHide ? hideCard(right) : vote(false))}
        >
          <img
            src={assetUrl(right.abs_path)}
            alt={right.filename}
            draggable={false}
            onClick={(e) => {
              e.stopPropagation()
              setLightbox(right.abs_path)
            }}
          />
          <Popover
            open={rightPopoverOpen}
            onOpenChange={setRightPopoverOpen}
            trigger={<span className="more-btn">更多 ▾</span>}
          >
            <ImageMetaPopover
              img={right}
              comparePrompt={bothOpen ? left.prompt_pos : undefined}
              diffColor="right"
              onHide={(img) => hideCard(img)}
            />
          </Popover>
          <div className="arena-score">{rightScore.toFixed(1)}</div>
          <div className="arena-label">{pendingHide ? '→ 屏蔽此卡' : '点击/→ 胜出'}</div>
        </div>
      </div>
      <p className="muted hint">
        {bindingDisplay(keybindings.arenaVoteLeft)} / {bindingDisplay(keybindings.arenaVoteRight)} 选胜方 ·{' '}
        {bindingDisplay(keybindings.arenaSkip)} 跳过 · 按住{' '}
        {bindingDisplay(keybindings.arenaArmHide)} 后按{' '}
        {bindingDisplay(keybindings.arenaHideLeft)} / {bindingDisplay(keybindings.arenaHideRight)}{' '}
        屏蔽对应卡 · {bindingDisplay(keybindings.arenaUndoHide)} 撤销最近屏蔽 · 单击图片放大 · 两侧"更多"可屏蔽并查看 Prompt 差异高亮
      </p>
      {lightbox && <Lightbox src={assetUrl(lightbox)} onClose={() => setLightbox(null)} />}
    </div>
  )
}
