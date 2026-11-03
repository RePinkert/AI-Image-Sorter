import { useState } from 'react'
import type { MouseEvent } from 'react'

/**
 * 缩略图 —— 外加「文件已删除」的显式状态（第 33 轮）。
 *
 * **为什么需要它**：用户在应用之外删掉文件后，图已经取不到了 ——
 * `<img>` 对着不存在的路径会触发 onError，而之前**没有任何东西接住
 * 它**：缩略图框里只剩纯黑 letterbox 衬底，看上去像「还在
 * 加载」，实际是「文件没了」。用户看到的现象就是「留了一个黑色占位符」。
 *
 * 这里把失败态画出来：底色 + 文字（不靠颜色单独传信息）。
 *
 * 两条判据，任一成立就是「缺失」：
 * 1. 后端已经知道（`fileMissing`）—— 实时 stat 出来的，不用等 404；
 * 2. `<img>` 自己加载失败 —— 封面 / 胶片条这些只有路径的地方靠这条。
 */
export interface ThumbProps {
  /** 已经转换好的 URL（桌面端 asset protocol / 浏览器端 dev bridge）。 */
  src: string
  alt?: string
  /** `<img>` 的类名 —— 尺寸与遮罩规则都挂在它身上。 */
  className?: string
  /** 后端已经知道文件不在盘上时为 true。 */
  fileMissing?: boolean
  /** 缺失标记里的文字。小格里塞不下长句。 */
  missingLabel?: string
  loading?: 'eager' | 'lazy'
  decoding?: 'async' | 'auto' | 'sync'
  draggable?: boolean
  onClick?: (event: MouseEvent<HTMLImageElement>) => void
}

export function Thumb({
  src,
  alt = '',
  className,
  fileMissing = false,
  missingLabel = '已删',
  loading,
  decoding,
  draggable,
  onClick,
}: ThumbProps) {
  // 记「失败的是哪个 src」而不是布尔 —— src 换了（新图）就自动恢复，
  // 不用 effect 去重置状态。
  const [failedSrc, setFailedSrc] = useState<string | null>(null)
  const missing = fileMissing || failedSrc === src
  if (missing) {
    return (
      <div
        className={`thumb-missing ${className ?? ''}`.trim()}
        role="img"
        aria-label="文件已删除"
        title="文件已不在原路径"
      >
        <span className="thumb-missing-label">{missingLabel}</span>
      </div>
    )
  }
  return (
    <img
      src={src}
      alt={alt}
      className={className}
      loading={loading}
      decoding={decoding}
      draggable={draggable}
      onClick={onClick}
      onError={() => setFailedSrc(src)}
    />
  )
}
