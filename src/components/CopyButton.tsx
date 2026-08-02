import { useState } from 'react'
import { copyText } from '../clipboard'

interface CopyButtonProps {
  text: string
  className?: string
  label?: string
  onCopied?: (success: boolean) => void
}

export function CopyButton({ text, className = '', label = '复制', onCopied }: CopyButtonProps) {
  const [done, setDone] = useState(false)

  return (
    <button
      type="button"
      className={`copy-btn ${className}`}
      title="复制到剪贴板"
      onClick={(e) => {
        e.stopPropagation()
        void copyText(text).then((ok) => {
          onCopied?.(ok)
          if (ok) {
            setDone(true)
            setTimeout(() => setDone(false), 1200)
          }
        })
      }}
    >
      {done ? '✓ 已复制' : label}
    </button>
  )
}
