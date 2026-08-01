import { useMemo } from 'react'

function tokenize(prompt: string): string[] {
  const lower = prompt.toLowerCase()
  const tokens: string[] = []
  for (const part of lower.split(/[\s,]+/)) {
    const t = part.trim()
    if (t) tokens.push(t)
  }
  return tokens
}

interface PromptDiffProps {
  left: string
  right: string
}

export function PromptDiff({ left, right }: PromptDiffProps) {
  // Computed unconditionally so the hook call order is stable even when
  // both prompts are empty (we still render the "no metadata" branch).
  const diff = useMemo(() => {
    const lSet = new Set(tokenize(left || ''))
    const rSet = new Set(tokenize(right || ''))
    const common = new Set([...lSet].filter((t) => rSet.has(t)))
    const lOnly = new Set([...lSet].filter((t) => !rSet.has(t)))
    const rOnly = new Set([...rSet].filter((t) => !lSet.has(t)))
    return { lOnly, rOnly, common }
  }, [left, right])

  const leftEmpty = !left?.trim()
  const rightEmpty = !right?.trim()

  if (leftEmpty && rightEmpty) {
    return (
      <div className="prompt-diff">
        <p className="muted">两张图均无 Prompt 元数据。</p>
      </div>
    )
  }

  const identical = (left || '').toLowerCase() === (right || '').toLowerCase()

  function renderTokens(text: string, lOnly: Set<string>, rOnly: Set<string>) {
    const tokens = text.split(/([\s,]+)/)
    return tokens.map((t, i) => {
      const key = t.toLowerCase()
      if (lOnly.has(key)) {
        return (
          <span key={i} className="diff-token diff-left">
            {t}
          </span>
        )
      }
      if (rOnly.has(key)) {
        return (
          <span key={i} className="diff-token diff-right">
            {t}
          </span>
        )
      }
      return <span key={i}>{t}</span>
    })
  }

  return (
    <div className="prompt-diff">
      {identical ? (
        <p className="muted">两图 Prompt 相同</p>
      ) : (
        <div className="prompt-diff-columns">
          <div className="prompt-diff-col">
            <div className="diff-label">左 Prompt（▲ 差异）</div>
            <pre className="diff-text">
              {leftEmpty ? '(空)' : renderTokens(left || '', diff.lOnly, diff.rOnly)}
            </pre>
          </div>
          <div className="prompt-diff-col">
            <div className="diff-label">右 Prompt（▲ 差异）</div>
            <pre className="diff-text">
              {rightEmpty ? '(空)' : renderTokens(right || '', diff.lOnly, diff.rOnly)}
            </pre>
          </div>
        </div>
      )}
      {diff.lOnly.size > 0 || diff.rOnly.size > 0 ? (
        <p className="hint">
          左独有 {diff.lOnly.size} token · 右独有 {diff.rOnly.size} token · 共有{' '}
          {diff.common.size} token
        </p>
      ) : null}
    </div>
  )
}
