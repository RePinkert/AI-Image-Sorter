import { useEffect, useMemo, useRef, useState, type ReactElement } from 'react'
import { errorMessage, recommendPrompts } from '../api'
import type { PromptRecommendation } from '../types'
import { track, trackError } from '../telemetry'
import { CopyButton } from './CopyButton'

interface Props {
  groupKey: string
  granularity: number
}

const PAGE_SIZE = 10

function tokenize(text: string): string[] {
  return text.toLowerCase().split(/[\s,]+/).map((part) => part.trim()).filter(Boolean)
}

function isWordToken(text: string): boolean {
  return /[a-zA-Z0-9\u4E00-\u9FFF]/.test(text)
}

function renderHighlighted(text: string, commonTokens: Set<string>): ReactElement[] {
  return text.split(/([\s,]+)/).map((part, index) => {
    const key = part.toLowerCase()
    if (!isWordToken(key)) return <span key={index}>{part}</span>
    return (
      <span key={index} className={commonTokens.has(key) ? 'rec-skeleton' : 'rec-highlight'}>
        {part}
      </span>
    )
  })
}

function displayLoras(item: PromptRecommendation) {
  if (item.loras.length === 0) return '无'
  return item.loras.map((lora) => {
    const clip = lora.clip_strength == null ? '' : ` / CLIP ${lora.clip_strength.toFixed(2)}`
    return `${lora.name} (${lora.strength.toFixed(2)}${clip})`
  }).join(' · ')
}

export function PromptRecommendPanel({ groupKey, granularity }: Props) {
  const [items, setItems] = useState<PromptRecommendation[]>([])
  const [offset, setOffset] = useState(0)
  const [hasMore, setHasMore] = useState(true)
  const [busy, setBusy] = useState(false)
  const [open, setOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const requestRef = useRef(0)

  useEffect(() => {
    requestRef.current += 1
    setItems([])
    setOffset(0)
    setHasMore(true)
    setOpen(false)
    setError(null)
  }, [groupKey, granularity])

  useEffect(() => () => {
    requestRef.current += 1
  }, [])

  const commonTokens = useMemo(() => {
    if (items.length < 2) return new Set<string>()
    const [first, ...rest] = items.map((item) => new Set(tokenize(item.prompt_text)))
    const intersection = new Set(first)
    for (const tokens of rest) {
      for (const token of intersection) {
        if (!tokens.has(token)) intersection.delete(token)
      }
    }
    return intersection
  }, [items])

  async function load(nextOffset: number) {
    if (busy) return
    const request = ++requestRef.current
    setBusy(true)
    setError(null)
    try {
      const rows = await recommendPrompts(groupKey, granularity, nextOffset, PAGE_SIZE)
      if (request !== requestRef.current) return
      setItems((previous) => nextOffset === 0 ? rows : [...previous, ...rows])
      setOffset(nextOffset + rows.length)
      setHasMore(rows.length === PAGE_SIZE)
    } catch (cause) {
      if (request !== requestRef.current) return
      setError(`加载推荐失败：${errorMessage(cause)}`)
      trackError('prompt_recommend_error', cause, {
        group_id: groupKey,
        retryable: cause != null && typeof cause === 'object' && 'retryable' in cause
          ? Boolean((cause as { retryable?: unknown }).retryable)
          : false,
      })
    } finally {
      if (request === requestRef.current) setBusy(false)
    }
  }

  function openPanel() {
    setOpen(true)
    track('prompt_recommend_open', { granularity, group_id: groupKey })
    if (items.length === 0) void load(0)
  }

  if (granularity >= 3) return null

  return (
    <>
      <button type="button" onClick={() => open ? setOpen(false) : openPanel()} disabled={busy && !open}>
        {open ? '收起推荐' : '分析 Prompt'}
      </button>
      {open && (
        <div className="prompt-recommend-panel">
          <div className="recommend-header">
            <strong>高分 Prompt 推荐</strong>
            <span className="hint">
              <span className="rec-highlight">高亮</span> = 变种词 · <span className="rec-skeleton">浅色</span> = 公共骨架
            </span>
          </div>
          {busy && items.length === 0 ? (
            <p className="muted" role="status">正在分析推荐…</p>
          ) : error && items.length === 0 ? (
            <div className="action-error" role="alert">
              <span>{error}</span>
              <button type="button" onClick={() => void load(0)}>重试</button>
            </div>
          ) : items.length === 0 ? (
            <p className="muted">暂无完整配方可推荐。</p>
          ) : (
            <div className="recommend-list">
              {items.map((item) => (
                <div className="recommend-item" key={`${item.prompt_text}:${item.checkpoint}:${item.sampler}`}>
                  <div className="recommend-scores">
                    <span className="rec-model">{item.diffusion_model || item.checkpoint || '模型未识别'}</span>
                    <span className="rec-max">最高 {item.max_score.toFixed(0)}</span>
                    <span className="rec-avg">均分 {item.avg_score.toFixed(1)} · 中位 {item.median_score.toFixed(1)}</span>
                    <span className="rec-count">{item.sample_count} 张 · 置信度 {(item.confidence * 100).toFixed(0)}%</span>
                    <CopyButton
                      text={item.prompt_text}
                      className="rec-copy-btn"
                      label="复制完整正 Prompt"
                      onCopied={(success) => track('prompt_recommend_copy', { success, group_id: groupKey })}
                    />
                  </div>
                  <pre className="recommend-text">
                    {commonTokens.size > 0 ? renderHighlighted(item.prompt_text, commonTokens) : item.prompt_text}
                  </pre>
                  {item.prompt_neg && (
                    <div className="recommend-reference">
                      <strong>Negative Prompt</strong>
                      <pre>{item.prompt_neg}</pre>
                    </div>
                  )}
                  <dl className="recommend-recipe">
                    <div><dt>Checkpoint</dt><dd>{item.checkpoint || '无'}</dd></div>
                    <div><dt>LoRA</dt><dd>{displayLoras(item)}</dd></div>
                    <div><dt>VAE</dt><dd>{item.vae || '默认'}</dd></div>
                    <div><dt>采样</dt><dd>{item.sampler} · {item.scheduler}</dd></div>
                    <div><dt>参数</dt><dd>{item.steps} steps · CFG {item.cfg.toFixed(2)}</dd></div>
                    <div><dt>尺寸</dt><dd>{item.width} × {item.height} · {item.aspect_ratio.toFixed(3)}</dd></div>
                    <div><dt>统计</dt><dd>方差 {item.score_variance.toFixed(2)} · 样本 {item.image_count}</dd></div>
                  </dl>
                </div>
              ))}
            </div>
          )}
          {error && items.length > 0 && <p className="action-error" role="alert">{error}</p>}
          {hasMore && items.length > 0 && (
            <button type="button" className="ghost recommend-more" disabled={busy} onClick={() => void load(offset)}>
              {busy ? '加载中…' : '加载更多'}
            </button>
          )}
        </div>
      )}
    </>
  )
}
