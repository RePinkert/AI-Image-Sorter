import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  assetUrl,
  confirmAction,
  errorMessage,
  listGroupImagesAll,
  splitImages,
  toggleHiddenAction,
  trashImage as trashImageApi,
} from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'
import { Lightbox } from './Lightbox'
import { PromptRecommendPanel } from './PromptRecommendPanel'
import { getTelemetrySessionId, trackAction } from '../telemetry'

type SortKey = 'score-desc' | 'score-asc' | 'filename' | 'seed' | 'size-desc' | 'size-asc'

export function FolderView() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const [images, setImages] = useState<ImageRow[]>([])
  const [busy, setBusy] = useState<number | null>(null)
  const [lightbox, setLightbox] = useState<string | null>(null)
  const [confirm, setConfirm] = useState<number | null>(null)
  const [sortKey, setSortKey] = useState<SortKey>('score-desc')
  const [showHiddenOnly, setShowHiddenOnly] = useState(false)
  const [batchMode, setBatchMode] = useState(false)
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState('')
  const [actionError, setActionError] = useState('')
  const loadRequestRef = useRef(0)

  const loadImages = useCallback(async () => {
    if (currentGroupKey == null) return
    const request = ++loadRequestRef.current
    setLoading(true)
    setLoadError('')
    try {
      const rows = await listGroupImagesAll(currentGroupKey, granularity)
      if (request !== loadRequestRef.current) return
      setImages(rows)
      setLoading(false)
    } catch (error) {
      if (request !== loadRequestRef.current) return
      setLoadError(errorMessage(error))
      setLoading(false)
    }
  }, [currentGroupKey, granularity])

  useEffect(() => {
    void loadImages()
    setSelected(new Set())
    setBatchMode(false)
    return () => {
      loadRequestRef.current += 1
    }
  }, [loadImages])

  const sorted = useMemo(() => {
    const arr = [...images]
    switch (sortKey) {
      case 'score-desc':
        arr.sort((a, b) => (b.score ?? 50) - (a.score ?? 50))
        break
      case 'score-asc':
        arr.sort((a, b) => (a.score ?? 50) - (b.score ?? 50))
        break
      case 'filename':
        arr.sort((a, b) => a.filename.localeCompare(b.filename))
        break
      case 'seed':
        arr.sort((a, b) => a.seed - b.seed)
        break
      case 'size-desc':
        arr.sort((a, b) => (b.size ?? 0) - (a.size ?? 0))
        break
      case 'size-asc':
        arr.sort((a, b) => (a.size ?? 0) - (b.size ?? 0))
        break
    }
    return arr
  }, [images, sortKey])

  const visible = useMemo(
    () => (showHiddenOnly ? sorted.filter((i) => i.hidden) : sorted),
    [sorted, showHiddenOnly],
  )

  async function toggleHidden(img: ImageRow) {
    if (busy !== null) return
    setBusy(img.id)
    setActionError('')
    const startedMs = Date.now()
    try {
      await toggleHiddenAction(img.id, !img.hidden, {
        sessionId: getTelemetrySessionId(),
        startedAt: new Date().toISOString(),
        contextSignature: currentGroupKey ?? undefined,
      })
      setImages((arr) =>
        arr.map((x) => (x.id === img.id ? { ...x, hidden: !img.hidden } : x)),
      )
      trackAction('hide', {
        hidden: !img.hidden,
        mode: 'folder',
        duration_ms: Date.now() - startedMs,
        image_id: img.id,
      })
    } catch (error) {
      setActionError(errorMessage(error))
    } finally {
      setBusy(null)
    }
  }

  async function trash(img: ImageRow) {
    if (busy !== null) return
    setBusy(img.id)
    setActionError('')
    setConfirm(null)
    try {
      await trashImageApi(img.id)
      setImages((arr) => arr.filter((x) => x.id !== img.id))
    } catch (e) {
      setActionError(`删除失败：${errorMessage(e)}`)
    } finally {
      setBusy(null)
    }
  }

  function toggleSelect(id: number) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  function selectAll() {
    setSelected(new Set(visible.map((i) => i.id)))
  }

  async function batchDelete() {
    const ids = Array.from(selected)
    if (ids.length === 0) return
    let ok = false
    try {
      ok = await confirmAction(
        `确定要删除选中的 ${ids.length} 张图片吗？`,
        '文件将送入系统回收站，可从桌面恢复。',
      )
    } catch (error) {
      setActionError(`无法打开确认窗口：${errorMessage(error)}`)
      return
    }
    if (!ok) return
    setBusy(-1)
    setActionError('')
    const succeeded = new Set<number>()
    try {
      for (const id of ids) {
        try {
          await trashImageApi(id)
          succeeded.add(id)
        } catch {
          // Keep failed rows selected so the user can retry them.
        }
      }
      setImages((arr) => arr.filter((x) => !succeeded.has(x.id)))
      const failedIds = ids.filter((id) => !succeeded.has(id))
      setSelected(new Set(failedIds))
      if (failedIds.length > 0) setActionError(`${failedIds.length} 张删除失败，可重试剩余项目。`)
    } finally {
      setBusy(null)
    }
  }

  async function batchUnhide() {
    const ids = Array.from(selected)
    if (ids.length === 0) return
    setBusy(-1)
    setActionError('')
    const succeeded = new Set<number>()
    try {
      for (const id of ids) {
        try {
          await toggleHiddenAction(id, false, {
            sessionId: getTelemetrySessionId(),
            startedAt: new Date().toISOString(),
            contextSignature: currentGroupKey ?? undefined,
          })
          succeeded.add(id)
          trackAction('hide', { hidden: false, mode: 'folder', image_id: id })
        } catch {
          // Keep failed rows selected so the user can retry them.
        }
      }
      setImages((arr) => arr.map((x) => (succeeded.has(x.id) ? { ...x, hidden: false } : x)))
      const failedIds = ids.filter((id) => !succeeded.has(id))
      setSelected(new Set(failedIds))
      if (failedIds.length > 0) setActionError(`${failedIds.length} 张取消屏蔽失败，可重试剩余项目。`)
    } finally {
      setBusy(null)
    }
  }

  // Manual 拆组: pull the selected images out of the current L2 group into
  // a brand-new group. They're pinned so future re-clustering won't re-absorb
  // them. Only meaningful at the Prompt偏差 (L2) granularity.
  async function batchSplit() {
    const ids = Array.from(selected)
    if (ids.length === 0 || granularity !== 2) return
    setBusy(-1)
    setActionError('')
    try {
      const r = await splitImages(2, ids)
      setImages((arr) => arr.filter((x) => !selected.has(x.id)))
      setSelected(new Set())
      alert(`已拆出 ${r.moved} 张为新分组`)
    } catch (e) {
      setActionError(`拆组失败：${errorMessage(e)}`)
    } finally {
      setBusy(null)
    }
  }

  if (currentGroupKey == null) {
    return (
      <div className="panel">
        <p>未选择分组。</p>
        <button type="button" onClick={() => setView('groups')}>返回</button>
      </div>
    )
  }

  if (loading) {
    return <div className="panel state-panel" role="status"><p className="muted">正在加载文件夹内容…</p></div>
  }

  if (loadError) {
    return (
      <div className="panel state-panel" role="alert">
        <p>加载失败：{loadError}</p>
        <button type="button" onClick={() => void loadImages()}>重试</button>
      </div>
    )
  }

  const summary = images.reduce(
    (acc, i) => {
      if (i.hidden) acc.hidden += 1
      else {
        acc.visible += 1
        const s = i.score ?? 50
        acc.sum += s
        acc.max = Math.max(acc.max, s)
        acc.min = Math.min(acc.min, s)
      }
      return acc
    },
    { visible: 0, hidden: 0, sum: 0, max: -Infinity, min: Infinity },
  )
  const avg = summary.visible > 0 ? summary.sum / summary.visible : 0

  return (
    <div className="folder-view">
      <div className="folder-topbar">
        <button type="button" onClick={() => setView('groups')}>← 返回分组</button>
        <span className="counter">文件夹视角</span>
        <span style={{ flex: 1 }} />
        {batchMode ? (
          <button type="button" onClick={() => { setBatchMode(false); setSelected(new Set()) }}>
            退出批量
          </button>
        ) : (
          <>
            <button type="button" onClick={() => setBatchMode(true)}>批量管理</button>
            <select
              className="sort-select"
              value={sortKey}
              onChange={(e) => setSortKey(e.target.value as SortKey)}
              title="排序方式"
            >
              <option value="score-desc">评分 ↓</option>
              <option value="score-asc">评分 ↑</option>
              <option value="filename">文件名</option>
              <option value="seed">Seed</option>
              <option value="size-desc">大小 ↓</option>
              <option value="size-asc">大小 ↑</option>
            </select>
          </>
        )}
        <button type="button" onClick={() => setView('swipe')}>滑卡模式</button>
        <button type="button" onClick={() => setView('arena')}>擂台模式</button>
      </div>
      <div className="folder-toolbar">
        共 {images.length} 张 · 可评分 {summary.visible} 张 · 已屏蔽 {summary.hidden} 张 ·
        平均 {avg.toFixed(1)} · 区间 {summary.visible > 0 ? `${summary.min.toFixed(0)}–${summary.max.toFixed(0)}` : '—'}
        <label className="hidden-filter" title="只看已屏蔽的图片">
          <input
            type="checkbox"
            checked={showHiddenOnly}
            onChange={(e) => setShowHiddenOnly(e.target.checked)}
          />
          只看已屏蔽
        </label>
        {batchMode && (
          <span className="batch-bar">
            已选 {selected.size} 张
            <button type="button" onClick={selectAll} disabled={busy !== null}>全选</button>
            <button type="button" onClick={() => void batchUnhide()} disabled={selected.size === 0 || busy !== null}>批量取消屏蔽</button>
            {granularity === 2 && (
              <button
                type="button"
                onClick={batchSplit}
                disabled={selected.size === 0 || busy !== null}
                title="将选中的图片拆出为新的 Prompt偏差 分组（自动重聚类不会重新并入）"
              >
                拆出为新分组
              </button>
            )}
            <button
              type="button"
              onClick={batchDelete}
              disabled={selected.size === 0 || busy !== null}
              style={{ borderColor: 'var(--bad)' }}
            >
              批量删除
            </button>
          </span>
        )}
        <PromptRecommendPanel groupKey={currentGroupKey} granularity={granularity} />
      </div>
      {actionError && <div className="action-error" role="alert"><span>{actionError}</span></div>}
      {visible.length === 0 ? (
        <div className="panel">
          <p className="muted">{showHiddenOnly ? '没有已屏蔽的图片。' : '该组无图片。'}</p>
        </div>
      ) : (
        <div className="folder-grid">
          {visible.map((img) => {
            const score = img.score ?? 50
            const isSelected = selected.has(img.id)
            return (
              <div
                className={`folder-item ${img.hidden ? 'blocked' : ''} ${batchMode ? 'batch' : ''} ${isSelected ? 'selected' : ''}`}
                key={img.id}
                onClick={batchMode ? () => toggleSelect(img.id) : undefined}
                role={batchMode ? 'button' : undefined}
                tabIndex={batchMode ? 0 : undefined}
                onKeyDown={batchMode ? (event) => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault()
                    toggleSelect(img.id)
                  }
                } : undefined}
              >
                {batchMode && (
                  <input
                    type="checkbox"
                    className="batch-check"
                    checked={isSelected}
                    onChange={(e) => { e.stopPropagation(); toggleSelect(img.id) }}
                    onClick={(e) => e.stopPropagation()}
                  />
                )}
                <img src={assetUrl(img.abs_path)} alt={img.filename} onClick={batchMode ? undefined : () => setLightbox(img.abs_path)} draggable={false} />
                <div className="folder-score" style={img.hidden ? { color: 'var(--muted)' } : undefined}>
                  {score.toFixed(0)}
                </div>
                {!batchMode && (
                  <div className="folder-actions">
                    <button
                      type="button"
                      onClick={(e) => { e.stopPropagation(); toggleHidden(img) }}
                      disabled={busy !== null}
                      title={img.hidden ? '取消屏蔽（重新参与评分）' : '屏蔽（赋 0 分，可恢复）'}
                    >
                      {img.hidden ? '取消屏蔽' : '屏蔽'}
                    </button>
                    <button
                      type="button"
                      onClick={(e) => { e.stopPropagation(); confirm === img.id ? trash(img) : setConfirm(img.id) }}
                      disabled={busy !== null}
                      title="送入系统回收站"
                      style={{ borderColor: 'var(--bad)' }}
                    >
                      {confirm === img.id ? '确认删除' : '删除'}
                    </button>
                  </div>
                )}
                {img.hidden && <div className="folder-blocked-tag">已屏蔽</div>}
              </div>
            )
          })}
        </div>
      )}
      <p className="muted hint" style={{ padding: '8px 16px' }}>
        单击图片放大 · 屏蔽可随时取消 · 删除送入系统回收站可从桌面恢复 · 批量管理支持全选 / 批量删除 / 批量取消屏蔽
      </p>
      {lightbox && <Lightbox src={assetUrl(lightbox)} onClose={() => setLightbox(null)} />}
    </div>
  )
}
