import { useMemo, type ReactElement } from 'react'
import type { ImageRow } from '../types'
import { CopyButton } from './CopyButton'

interface ImageMetaPopoverProps {
  img: ImageRow
  comparePrompt?: string
  diffColor?: 'left' | 'right'
  /** When provided, renders a "屏蔽" action at the bottom of the popover. */
  onHide?: (img: ImageRow) => void
}

function tokenize(text: string): string[] {
  const lower = text.toLowerCase()
  const tokens: string[] = []
  for (const part of lower.split(/[\s,]+/)) {
    const t = part.trim()
    if (t) tokens.push(t)
  }
  return tokens
}

function renderPromptWithDiff(
  text: string,
  otherText: string,
  color: 'left' | 'right',
): ReactElement {
  const curSet = new Set(tokenize(text))
  const otherSet = new Set(tokenize(otherText))
  const onlyHere = new Set([...curSet].filter((t) => !otherSet.has(t)))
  if (onlyHere.size === 0) return <>{text}</>

  const cls = color === 'left' ? 'diff-left' : 'diff-right'
  const parts = text.split(/([\s,]+)/)
  return (
    <>
      {parts.map((p, i) => {
        const key = p.toLowerCase()
        if (onlyHere.has(key)) {
          return (
            <span key={i} className={`diff-token ${cls}`}>
              {p}
            </span>
          )
        }
        return <span key={i}>{p}</span>
      })}
    </>
  )
}

export function ImageMetaPopover({ img, comparePrompt, diffColor, onHide }: ImageMetaPopoverProps) {
  const promptDisplay = useMemo(() => {
    const text = img.prompt_pos || '(无)'
    if (!comparePrompt || !img.prompt_pos) return <>{text}</>
    return renderPromptWithDiff(img.prompt_pos, comparePrompt, diffColor ?? 'left')
  }, [img.prompt_pos, comparePrompt, diffColor])

  return (
    <div className="meta-popover-content">
      {img.manually_grouped && (
        <div className={`manual-badge manual-${img.manually_grouped}`}>
          {img.manually_grouped === 'split'
            ? '本图已手动拆出为新分组（自动重聚类不会重新并入）'
            : '本组为手动合并（Prompt偏差），自动重聚类不会拆开'}
        </div>
      )}
      <div className="meta-field">
        <strong>
          正 prompt
          {comparePrompt && (
            <span className="diff-badge" data-side={diffColor}>
              {diffColor === 'left' ? '左独有' : '右独有'}高亮
            </span>
          )}
          <CopyButton text={img.prompt_pos || ''} className="meta-copy-btn" />
        </strong>
        <pre>{promptDisplay}</pre>
      </div>
      <div className="meta-field">
        <strong>负 prompt</strong>
        <pre>{img.prompt_neg || '(无)'}</pre>
      </div>
      <div className="meta-field">
        <strong>模型</strong>
        <pre>{img.checkpoint || '(无)'}</pre>
      </div>
      {img.diffusion_model && img.diffusion_model !== img.checkpoint && (
        <div className="meta-field">
          <strong>Diffusion Model</strong>
          <pre>{img.diffusion_model}</pre>
        </div>
      )}
      {img.workflow_name && (
        <div className="meta-field">
          <strong>工作流</strong>
          <pre>{img.workflow_name}</pre>
        </div>
      )}
      <div className="meta-field">
        <strong>LoRA</strong>
        <pre>{img.loras || '(无)'}</pre>
      </div>
      <div className="meta-field">
        <strong>VAE</strong>
        <pre>{img.vae || '(默认)'}</pre>
      </div>
      {img.samplers && (
        <div className="meta-field">
          <strong>Sampler 参数</strong>
          <pre>{img.samplers}</pre>
        </div>
      )}
      <div className="meta-row">
        <span><strong>Seed</strong> {img.seed}</span>
        <span><strong>尺寸</strong> {img.width ?? '?'}×{img.height ?? '?'}</span>
      </div>
      <div className="meta-row">
        <span><strong>类型</strong> {img.source_kind || '—'}</span>
        <span><strong>{img.meta_ok ? '元数据完整' : '元数据缺失'}</strong></span>
      </div>
      {onHide && (
        <button
          type="button"
          className="meta-hide-btn"
          onClick={() => onHide(img)}
          title="屏蔽（赋 0 分，不参与评分，可在文件夹视角恢复）"
        >
          屏蔽此图
        </button>
      )}
    </div>
  )
}
