import { useEffect, useMemo, useState, type ReactElement } from 'react'
import { recommendPrompts } from '../api'
import type { PromptRecommendation } from '../types'
import { CopyButton } from './CopyButton'

interface Props {
  groupKey: string
  granularity: number
}

const PAGE_SIZE = 10

function tokenize(text: string): string[] {
  const lower = text.toLowerCase()
  const tokens: string[] = []
  for (const part of lower.split(/[\s,]+/)) {
    const t = part.trim()
    if (t) tokens.push(t)
  }
  return tokens
}

function isWordToken(s: string): boolean {
  return /[a-zA-Z0-9\u4E00-\u9FFF]/.test(s)
}

function renderHighlighted(
  text: string,
  commonTokens: Set<string>,
): ReactElement[] {
  const parts = text.split(/([\s,]+)/)
  return parts.map((p, i) => {
    const key = p.toLowerCase()
    if (!isWordToken(key)) return <span key={i}>{p}</span>
    if (commonTokens.has(key)) {
      return (
        <span key={i} className="rec-skeleton">
          {p}
        </span>
      )
    }
    return (
      <span key={i} className="rec-highlight">
        {p}
      </span>
    )
  })
}

export function PromptRecommendPanel({ groupKey, granularity }: Props) {
  const [items, setItems] = useState<PromptRecommendation[]>([])
  const [offset, setOffset] = useState(0)
  const [hasMore, setHasMore] = useState(true)
  const [busy, setBusy] = useState(false)
  const [open, setOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setItems([])
    setOffset(0)
    setHasMore(true)
    setOpen(false)
    setError(null)
  }, [groupKey, granularity])

  const commonTokens = useMemo(() => {
    const texts = items.map((r) => r.prompt_text)
    if (texts.length < 2) return new Set<string>()
    const tokenSets = texts.map((t) => new Set(tokenize(t)))
    const [first, ...rest] = tokenSets
    const inter = new Set(first)
    for (const s of rest) {
      for (const t of inter) {
        if (!s.has(t)) inter.delete(t)
      }
    }
    return inter
  }, [items])

  async function load(off: number) {
    setBusy(true)
    setError(null)
    try {
      const rows = await recommendPrompts(groupKey, granularity, off, PAGE_SIZE)
      if (off === 0) {
        setItems(rows)
      } else {
        setItems((prev) => [...prev, ...rows])
      }
      setOffset(off + rows.length)
      if (rows.length < PAGE_SIZE) setHasMore(false)
    } catch (e) {
      setError(`加载推荐失败：${String(e)}`)
    } finally {
      setBusy(false)
    }
  }

  async function openPanel() {
    setOpen(true)
    if (items.length === 0) load(0)
  }

  if (granularity >= 3) return null

  return (
    <>
      <button onClick={() => (open ? setOpen(false) : openPanel())} disabled={busy}>
        {open ? '收起推荐' : '分析 Prompt'}
      </button>
      {open && (
        <div className="prompt-recommend-panel">
          <div className="recommend-header">
            <strong>高分 Prompt 推荐</strong>
            <span className="hint">
              按最高评分排序 ·{' '}
              <span className="rec-highlight">高亮</span> = 变种词 ·{' '}
              <span className="rec-skeleton">浅色</span> = 公共骨架
            </span>
          </div>
          {error ? (
            <p className="msg">{error}</p>
          ) : items.length === 0 && !busy ? (
            <p className="muted">暂无可推荐数据。</p>
          ) : (
            <div className="recommend-list">
              {items.map((r, i) => (
                <div className="recommend-item" key={i}>
                  <div className="recommend-scores">
                    <span className="rec-model">{r.diffusion_model || '模型未识别'}</span>
                    <span className="rec-max">最高 {r.max_score.toFixed(0)}</span>
                    <span className="rec-avg">均分 {r.avg_score.toFixed(1)}</span>
                    <span className="rec-count">{r.image_count} 张</span>
                    <CopyButton text={r.prompt_text} className="rec-copy-btn" />
                  </div>
                  <pre className="recommend-text">
                    {commonTokens.size > 0
                      ? renderHighlighted(r.prompt_text, commonTokens)
                      : r.prompt_text}
                  </pre>
                </div>
              ))}
            </div>
          )}
          {hasMore && (
            <button
              className="ghost"
              disabled={busy}
              onClick={() => load(offset)}
              style={{ width: '100%', marginTop: 8 }}
            >
              {busy ? '加载中...' : '加载更多'}
            </button>
          )}
        </div>
      )}
    </>
  )
}
