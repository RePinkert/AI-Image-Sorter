import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  assetUrl,
  confirmAction,
  errorMessage,
  listGroupImagesAll,
  listMoveTargets,
  moveImagesToGroup,
  undoSplit,
  getGroupThumbnails,
  splitImages,
  toggleHiddenAction,
  trashImage as trashImageApi,
  unmergeGroup,
} from '../api'
import type { ImageRow } from '../types'
import { useStore } from '../store'
import { Lightbox } from './Lightbox'
import { PromptRecommendPanel } from './PromptRecommendPanel'
import { getTelemetrySessionId, trackAction } from '../telemetry'

type SortKey =
  | 'score-desc'
  | 'score-asc'
  | 'filename'
  | 'seed'
  | 'size-desc'
  | 'size-asc'
  | 'modified-desc'
  | 'modified-asc'

export function FolderView() {
  const setView = useStore((s) => s.setView)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const currentSourceId = useStore((s) => s.currentSourceId)
  const granularity = useStore((s) => s.granularity)
  const bumpDataRevision = useStore((s) => s.bumpDataRevision)
  const [images, setImages] = useState<ImageRow[]>([])
  const [busy, setBusy] = useState<number | null>(null)
  const [lightbox, setLightbox] = useState<ImageRow | null>(null)
  const [confirm, setConfirm] = useState<number | null>(null)
  const [sortKey, setSortKey] = useState<SortKey>('score-desc')
  const [showHiddenOnly, setShowHiddenOnly] = useState(false)
  const [batchMode, setBatchMode] = useState(false)
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState('')
  const [actionError, setActionError] = useState('')
  const loadRequestRef = useRef(0)
  const gridRef = useRef<HTMLDivElement | null>(null)
  const [masonry, setMasonry] = useState<{ items: { top: number; left: number; w: number; h: number }[]; totalH: number } | null>(null)
  const [moveTargets, setMoveTargets] = useState<import('../types').GroupInfo[] | null>(null)
  const [moveThumbs, setMoveThumbs] = useState<Record<string, string[]>>({})
  const [moveBusy, setMoveBusy] = useState(false)


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
      case 'modified-desc':
        arr.sort((a, b) => (b.modified_at ?? 0) - (a.modified_at ?? 0))
        break
      case 'modified-asc':
        arr.sort((a, b) => (a.modified_at ?? 0) - (b.modified_at ?? 0))
        break
    }
    return arr
  }, [images, sortKey])

  const visible = useMemo(
    () => (showHiddenOnly ? sorted.filter((i) => i.hidden) : sorted),
    [sorted, showHiddenOnly],
  )

  // Waterfall (masonry) layout: every card gets its image's natural aspect
  // ratio (from the DB width/height, so no waiting for image load), and the
  // next card lands in the currently shortest column. Sparse groups (e.g. an
  // L2 group with 1–2 images) and mixed aspect ratios therefore never get
  // letterboxed black bars or cropped thumbs. Pure-CSS grid remains as the
  // pre-layout / fallback render.
  useEffect(() => {
    const el = gridRef.current
    if (!el || visible.length === 0) {
      setMasonry(null)
      return
    }
    const compute = () => {
      const gap = 10
      const padX = 16
      const padY = 14
      const avail = el.clientWidth - padX * 2
      if (avail <= 0) return
      const min = 180
      const cols = Math.max(1, Math.floor((avail + gap) / (min + gap)))
      const colW = (avail - (cols - 1) * gap) / cols
      const hs = new Array<number>(cols).fill(0)
      const items = visible.map((img) => {
        const ratio = img.width && img.height ? img.width / img.height : 1
        const h = colW / ratio
        const col = hs.indexOf(Math.min(...hs))
        const pos = { top: hs[col] + padY, left: col * (colW + gap) + padX, w: colW, h }
        hs[col] += h + gap
        return pos
      })
      setMasonry({ items, totalH: Math.max(0, Math.max(...hs) - gap) + padY * 2 })
    }
    compute()
    const ro = new ResizeObserver(compute)
    ro.observe(el)
    return () => ro.disconnect()
  }, [visible])

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

  async function openMoveDialog() {
    if (!currentGroupKey || currentSourceId == null || selected.size === 0) return
    try {
      const targets = await listMoveTargets(currentSourceId, currentGroupKey)
      setMoveTargets(targets)
      const thumbs = await getGroupThumbnails(targets.map((g) => g.group_key), 2)
      setMoveThumbs(Object.fromEntries(thumbs.map((x) => [x.group_key, x.thumb_paths])))
    } catch (e) { setActionError(`读取目标分组失败：${errorMessage(e)}`) }
  }

  async function undoSplitGroup() {
    if (currentGroupKey == null || currentSourceId == null) return
    setBusy(-1)
    try {
      await undoSplit(currentGroupKey, currentSourceId)
      bumpDataRevision()
      setView('groups')
    } catch (e) { setActionError(`撤销拆组失败：${errorMessage(e)}`) }
    finally { setBusy(null) }
  }

  async function moveSelected(target: string) {
    setMoveBusy(true)
    try {
      await moveImagesToGroup(Array.from(selected), target)
      setImages((arr) => arr.filter((img) => !selected.has(img.id)))
      setSelected(new Set()); setMoveTargets(null); bumpDataRevision()
    } catch (e) { setActionError(`移动失败：${errorMessage(e)}`) }
    finally { setMoveBusy(false) }
  }

  // This folder view is the rollback point for a manual L2 merge: every
  // member of a merged group carries a kind='merge' binding, so the group
  // itself offers the undo. Restores each image to its pre-merge group.
  async function undoMerge() {
    if (currentGroupKey == null) return
    const mergedCount = images.filter((i) => i.manually_grouped === 'merge').length
    let ok = false
    try {
      ok = await confirmAction(
        `撤销合并「${mergedCount} 张图片所在组」？`,
        '这些图片将回到合并前的原分组，绑定关系将被移除（自动重聚类可重新分组）。'
      )
    } catch (error) {
      setActionError(`无法打开确认窗口：${errorMessage(error)}`)
      return
    }
    if (!ok) return
    setBusy(-1)
    setActionError('')
    try {
      const restored = await unmergeGroup(currentGroupKey, currentSourceId)
      alert(`已撤销合并：${restored} 张图片回到原分组`)
      bumpDataRevision()
      setView('groups')
    } catch (e) {
      setActionError(`撤销合并失败：${errorMessage(e)}`)
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

  // A manual L2 merge pins every member with kind='merge'; when this
  // folder view sits on such a group it becomes the rollback point.
  const mergedCount = granularity === 2
    ? images.filter((i) => i.manually_grouped === 'merge').length
    : 0
  const splitCount = granularity === 2
    ? images.filter((i) => i.manually_grouped === 'split').length
    : 0

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
              <option value="modified-desc">修改时间 ↓</option>
              <option value="modified-asc">修改时间 ↑</option>
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
        {granularity === 2 && mergedCount > 0 && (
          <button
            type="button"
            className="unmerge-btn"
            disabled={busy !== null}
            onClick={() => void undoMerge()}
            title="该组为手动合并结果：撤销合并，让图片回到合并前的原分组"
          >
            撤销合并（{mergedCount} 张）
          </button>
        )}
        {granularity === 2 && splitCount > 0 && (
          <button type="button" disabled={busy !== null} onClick={() => void undoSplitGroup()} title="撤销当前手动拆出的图片并恢复原 L2 分组">
            撤销拆组（{splitCount} 张）
          </button>
        )}
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
            {granularity === 2 && <button type="button" onClick={() => void openMoveDialog()} disabled={selected.size === 0 || busy !== null}>移动至其他组</button>}
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
        <div
          className="folder-grid"
          ref={gridRef}
          style={masonry ? { height: masonry.totalH } : undefined}
        >
          {visible.map((img, index) => {
            const score = img.score ?? 50
            const isSelected = selected.has(img.id)
            const pos = masonry?.items[index]
            return (
              <div
                className={`folder-item ${img.hidden ? 'blocked' : ''} ${batchMode ? 'batch' : ''} ${isSelected ? 'selected' : ''}`}
                key={img.id}
                style={pos ? { position: 'absolute', top: pos.top, left: pos.left, width: pos.w, height: pos.h, minHeight: 0 } : undefined}
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
                <img src={assetUrl(img.abs_path)} alt={img.filename} onClick={batchMode ? undefined : () => setLightbox(img)} draggable={false} />
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
      {lightbox && (
        <Lightbox src={assetUrl(lightbox.abs_path)} meta={lightbox} onClose={() => setLightbox(null)} />
      )}
      {moveTargets && (
        <div className="merge-confirm-overlay" onClick={() => !moveBusy && setMoveTargets(null)}>
          <div className="merge-confirm" role="dialog" aria-modal="true" onClick={(e) => e.stopPropagation()}>
            <h3>移动 {selected.size} 张图片至其他 Prompt偏差组</h3>
            {moveTargets.length === 0 ? <p className="muted">当前来源目录下没有其他可用分组。</p> : moveTargets.map((g) => (
              <button key={g.group_key} className="move-target-card" type="button" disabled={moveBusy} onClick={() => void moveSelected(g.group_key)}>
                <span className="move-target-thumbs">{(moveThumbs[g.group_key] ?? []).slice(0, 4).map((path) => <img key={path} src={assetUrl(path)} alt="" />)}</span>
                <span className="move-target-info"><strong>{g.prompt_pos.slice(0, 100) || '(无 prompt)'}</strong><span>{g.count} 张 · {g.checkpoint || '未知模型'} · {g.workflow_name || '未命名工作流'}</span></span>
              </button>
            ))}
            <button type="button" onClick={() => setMoveTargets(null)} disabled={moveBusy}>取消</button>
          </div>
        </div>
      )}
    </div>
  )
}
