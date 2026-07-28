import { useEffect, useState } from 'react'
import { arenaVote, assetUrl, listGroupImages } from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'

export function Arena() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const [images, setImages] = useState<ImageRow[]>([])
  const [left, setLeft] = useState<ImageRow | null>(null)
  const [right, setRight] = useState<ImageRow | null>(null)
  const [scores, setScores] = useState<Record<number, number>>({})
  const [busy, setBusy] = useState(false)

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
  }, [currentGroupKey, granularity])

  async function pickPair(imgs: ImageRow[], sc: Record<number, number>) {
    for (let i = 0; i < imgs.length; i++) {
      for (let j = i + 1; j < imgs.length; j++) {
        const a = imgs[i]
        const b = imgs[j]
        const sa = sc[a.id] ?? 50
        const sb = sc[b.id] ?? 50
        if (Math.abs(sa - sb) < 5) {
          setLeft(a)
          setRight(b)
          return
        }
      }
    }
    if (imgs.length >= 2) {
      setLeft(imgs[0])
      setRight(imgs[1])
    } else {
      setLeft(null)
      setRight(null)
    }
  }

  async function vote(winnerIsLeft: boolean) {
    if (!left || !right || !currentGroupKey || busy) return
    setBusy(true)
    const [nl, nr] = await arenaVote(currentGroupKey, left.id, right.id, winnerIsLeft)
    setScores((s) => ({ ...s, [left.id]: nl, [right.id]: nr }))
    setBusy(false)
    pickPair(images, { ...scores, [left.id]: nl, [right.id]: nr })
  }

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
        <button onClick={() => pickPair(images, scores)}>下一对</button>
      </div>
      <div className="arena-stage">
        <div className="arena-card" onClick={() => vote(true)}>
          <img src={assetUrl(left.abs_path)} alt={left.filename} draggable={false} />
          <div className="arena-score">{leftScore.toFixed(1)}</div>
          <div className="arena-label">点击胜出</div>
        </div>
        <div className="arena-vs">VS</div>
        <div className="arena-card" onClick={() => vote(false)}>
          <img src={assetUrl(right.abs_path)} alt={right.filename} draggable={false} />
          <div className="arena-score">{rightScore.toFixed(1)}</div>
          <div className="arena-label">点击胜出</div>
        </div>
      </div>
      <p className="muted hint">点击图片选择胜者，分数将按 Bradley-Terry 收敛更新。</p>
    </div>
  )
}
