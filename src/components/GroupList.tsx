import { useEffect, useRef, useState } from 'react'
import { assetUrl, errorMessage, getGroupThumbnails, listGroups, mergeGroups } from '../api'
import type { Granularity, GroupInfo, GroupThumbDto } from '../types'
import { useStore } from '../store'

const LEVELS: { value: Granularity; label: string }[] = [
  { value: 0, label: '文件夹' },
  { value: 1, label: '工作流' },
  { value: 2, label: 'Prompt偏差' },
  { value: 3, label: '独立Prompt' },
]

export function GroupList() {
  const setView = useStore((s) => s.setView)
  const groups = useStore((s) => s.groups)
  const setGroups = useStore((s) => s.setGroups)
  const setCurrentGroupKey = useStore((s) => s.setCurrentGroupKey)
  const currentSourceId = useStore((s) => s.currentSourceId)
  const setCurrentSourceId = useStore((s) => s.setCurrentSourceId)
  const setL2Threshold = useStore((s) => s.setL2Threshold)
  const granularity = useStore((s) => s.granularity)
  const setGranularity = useStore((s) => s.setGranularity)
  const sources = useStore((s) => s.sources)
  const dataRevision = useStore((s) => s.dataRevision)
  const [thumbs, setThumbs] = useState<Record<string, string[]>>({})
  const [loadingThumbs, setLoadingThumbs] = useState(false)
  const [mergeMode, setMergeMode] = useState(false)
  const [selectedKeys, setSelectedKeys] = useState<Set<string>>(new Set())
  const [mergeBusy, setMergeBusy] = useState(false)
  const [mergeMsg, setMergeMsg] = useState<string | null>(null)
  const [loadingGroups, setLoadingGroups] = useState(groups.length === 0)
  const [groupError, setGroupError] = useState('')
  const [thumbError, setThumbError] = useState('')
  const groupRequestRef = useRef(0)
  const thumbRequestRef = useRef(0)

  async function loadGroups(sourceId: number | null, level: Granularity) {
    const request = ++groupRequestRef.current
    setCurrentSourceId(sourceId)
    setLoadingGroups(true)
    setGroupError('')
    // The source's persisted threshold is authoritative — keep the Settings
    // slider / merge suggestions in sync with what the backend actually
    // re-clusters at.
    if (sourceId != null) {
      const src = sources.find((s) => s.id === sourceId)
      if (src?.l2_threshold != null) setL2Threshold(src.l2_threshold)
    }
    try {
      const g = await listGroups(sourceId ?? undefined, level)
      if (request !== groupRequestRef.current) return
      setGroups(g)
      setThumbs({})
    } catch (error) {
      if (request !== groupRequestRef.current) return
      setGroupError(errorMessage(error))
    } finally {
      if (request === groupRequestRef.current) setLoadingGroups(false)
    }
  }

  useEffect(() => {
    if (groups.length === 0 && sources.length > 0) {
      void loadGroups(currentSourceId ?? sources[0].id, granularity)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sources])

  useEffect(() => {
    if (dataRevision > 0) void loadGroups(currentSourceId, granularity)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataRevision])

  useEffect(() => () => {
    groupRequestRef.current += 1
    thumbRequestRef.current += 1
  }, [])

  useEffect(() => {
    if (groups.length > 0) {
      loadThumbs(groups)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [groups])

  async function loadThumbs(gs: GroupInfo[]) {
    if (gs.length === 0) return
    const request = ++thumbRequestRef.current
    setLoadingThumbs(true)
    setThumbError('')
    const keys = gs.map((g) => g.group_key)
    try {
      const dtos: GroupThumbDto[] = await getGroupThumbnails(keys, granularity)
      const map: Record<string, string[]> = {}
      dtos.forEach((d) => {
        map[d.group_key] = d.thumb_paths
      })
      if (request === thumbRequestRef.current) setThumbs(map)
    } catch (error) {
      if (request === thumbRequestRef.current) setThumbError(errorMessage(error))
    } finally {
      if (request === thumbRequestRef.current) setLoadingThumbs(false)
    }
  }

  function onLevelChange(level: Granularity) {
    setGranularity(level)
    if (level !== 2) exitMergeMode()
    void loadGroups(currentSourceId, level)
  }

  function exitMergeMode() {
    setMergeMode(false)
    setSelectedKeys(new Set())
    setMergeMsg(null)
  }

  function toggleMergeSelect(key: string) {
    setSelectedKeys((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  async function reloadGroupsAtLevel() {
    if (currentSourceId == null) return
    await loadGroups(currentSourceId, granularity)
  }

  async function doMerge() {
    const keys = Array.from(selectedKeys)
    if (keys.length < 2 || mergeBusy) return
    setMergeBusy(true)
    setMergeMsg(null)
    try {
      const r = await mergeGroups(2, keys)
      setMergeMsg(`已合并 ${r.moved} 张到同一分组（自动重聚类不会再拆开）`)
      setSelectedKeys(new Set())
      await reloadGroupsAtLevel()
    } catch (e) {
      setMergeMsg(`合并失败：${errorMessage(e)}`)
    } finally {
      setMergeBusy(false)
    }
  }

  function openGroup(g: GroupInfo) {
    setCurrentGroupKey(g.group_key)
    setView('swipe')
  }

  function openFolder(g: GroupInfo) {
    setCurrentGroupKey(g.group_key)
    setView('folder')
  }

  function displayLabel(g: GroupInfo): string {
    if (granularity === 0) {
      return g.source_path || '(文件夹)'
    }
    if (granularity === 1) {
      return g.workflow_name || '未命名工作流'
    }
    return g.prompt_pos.slice(0, 120) || '(无 prompt)'
  }

  const syncProgress = useStore((s) => s.syncProgress)
  const syncing = syncProgress.active && syncProgress.pending > 0

  return (
    <div className="panel">
      <h2>分组列表</h2>
      <div className="row">
        <select
          value={currentSourceId ?? ''}
          onChange={(e) => {
            if (currentSourceId !== (e.target.value ? Number(e.target.value) : null)) {
              exitMergeMode()
            }
            void loadGroups(e.target.value ? Number(e.target.value) : null, granularity)
          }}
        >
          <option value="">所有源</option>
          {sources.map((s) => (
            <option key={s.id} value={s.id}>
              {s.alias ?? s.path}
            </option>
          ))}
        </select>
        <button type="button" onClick={() => setView('import')}>+ 导入新源</button>
        <button type="button" onClick={() => setView('settings')}>标签设置 / 导出</button>
        {granularity === 2 && (
          <button
            type="button"
            className={mergeMode ? 'gran-active' : ''}
            disabled={mergeBusy}
            onClick={() => (mergeMode ? exitMergeMode() : setMergeMode(true))}
            title="勾选两个及以上 Prompt偏差 组，合并为同一组（自动重聚类不会拆开）"
          >
            {mergeMode ? '合并分组…' : '合并分组'}
          </button>
        )}
      </div>

      <div className="granularity-bar">
        {LEVELS.map((l) => (
          <button
            type="button"
            key={l.value}
            className={granularity === l.value ? 'gran-active' : ''}
            onClick={() => onLevelChange(l.value)}
          >
            {l.label}
          </button>
        ))}
      </div>

      {loadingGroups && <p className="muted" role="status">正在加载分组…</p>}
      {groupError && (
        <div className="action-error" role="alert">
          <span>分组加载失败：{groupError}</span>
          <button type="button" onClick={() => void loadGroups(currentSourceId, granularity)}>重试</button>
        </div>
      )}
      {!loadingGroups && !groupError && groups.length === 0 && <p className="muted">暂无分组，请先导入。</p>}
      {loadingThumbs && groups.length > 0 && (
        <p className="muted hint">读取组预览中…</p>
      )}
      {thumbError && <p className="muted hint" role="alert">组预览加载失败：{thumbError}</p>}
      {syncing && (
        <p className="sync-warn">扫描进行中，分组数据可能不完整。</p>
      )}
      {mergeMode && granularity === 2 && (
        <div className="merge-bar">
          <span>已选 {selectedKeys.size} 组（至少选 2 组）</span>
          <button type="button" disabled={mergeBusy || selectedKeys.size < 2} onClick={() => void doMerge()}>
            合并所选
          </button>
          <button type="button" className="ghost" disabled={mergeBusy} onClick={exitMergeMode}>
            退出
          </button>
          {mergeMsg && <span className="merge-msg">{mergeMsg}</span>}
        </div>
      )}
      <div className="group-grid">
        {groups.map((g) => (
          <div
            key={g.group_key}
            className={`group-card ${mergeMode && granularity === 2 && selectedKeys.has(g.group_key) ? 'merge-selected' : ''}`}
            onClick={() => (mergeMode && granularity === 2 ? toggleMergeSelect(g.group_key) : openGroup(g))}
            role="button"
            tabIndex={0}
            onKeyDown={(event) => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault()
                mergeMode && granularity === 2 ? toggleMergeSelect(g.group_key) : openGroup(g)
              }
            }}
            title={displayLabel(g)}
          >
            {mergeMode && granularity === 2 && (
              <input
                type="checkbox"
                className="merge-check"
                checked={selectedKeys.has(g.group_key)}
                onChange={(e) => {
                  e.stopPropagation()
                  toggleMergeSelect(g.group_key)
                }}
                onClick={(e) => e.stopPropagation()}
              />
            )}
            <div className="group-thumb">
              {thumbs[g.group_key]?.length ? (
                <div className={`group-thumb-grid count-${Math.min(thumbs[g.group_key].length, 4)}`}>
                  {thumbs[g.group_key].map((path, index) => (
                    <img key={`${path}-${index}`} src={assetUrl(path)} alt="" draggable={false} />
                  ))}
                </div>
              ) : (
                <div className="thumb-placeholder" />
              )}
              <span className="group-count-badge">{g.count}</span>
            </div>
            <div className="group-prompt">{displayLabel(g)}</div>
            {granularity === 2 && g.manually_merged && (
              <span className="merged-badge" title="该组为手动合并结果，自动重聚类不会拆开">
                已手动合并
              </span>
            )}
            <div className="group-meta">
              {g.checkpoint || '—'} · {g.source_kind}
              <button
                type="button"
                className="folder-link"
                onClick={(e) => {
                  e.stopPropagation()
                  openFolder(g)
                }}
                title="文件夹视角（速览评分 / 屏蔽 / 删除）"
                style={{ marginLeft: 8 }}
              >
                文件夹视角
              </button>
            </div>
            {granularity === 1 && g.model_facets && g.model_facets.length > 0 && (
              <div className="model-facets">
                {g.model_facets.map((f) => (
                  <span key={f.model} className="model-chip" title={f.model}>
                    {f.model} <b>{f.count}</b>
                  </span>
                ))}
              </div>
            )}
          </div>
        ))}
      </div>
    </div>
  )
}
