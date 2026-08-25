import { useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'framer-motion'
import { assetUrl, errorMessage, getGroupThumbnails, listGroups, mergeGroups } from '../api'
import type { Granularity, GroupInfo, GroupThumbDto } from '../types'
import { useStore } from '../store'

const LEVELS: { value: Granularity; label: string }[] = [
  { value: 0, label: '文件夹' },
  { value: 1, label: 'Model偏差' },
  { value: 2, label: 'Prompt偏差' },
  { value: 3, label: '独立Prompt' },
]

const groupThumbCache = new Map<string, string[]>()
const groupThumbInflight = new Map<string, Promise<string[]>>()

function LazyGroupThumb({ groupKey, level }: { groupKey: string; level: Granularity }) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const [paths, setPaths] = useState<string[]>(() => groupThumbCache.get(`${level}:${groupKey}`) ?? [])
  const [visible, setVisible] = useState(false)

  useEffect(() => {
    const el = hostRef.current
    if (!el) return
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setVisible(true)
          observer.disconnect()
        }
      },
      { rootMargin: '600px 0px' },
    )
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  useEffect(() => {
    if (!visible || paths.length > 0) return
    const cacheKey = `${level}:${groupKey}`
    let request = groupThumbInflight.get(cacheKey)
    if (!request) {
      request = getGroupThumbnails([groupKey], level).then((rows: GroupThumbDto[]) => rows[0]?.thumb_paths ?? [])
      groupThumbInflight.set(cacheKey, request)
    }
    let cancelled = false
    void request.then((next) => {
      groupThumbCache.set(cacheKey, next)
      groupThumbInflight.delete(cacheKey)
      if (!cancelled) setPaths(next)
    }).catch(() => {
      groupThumbInflight.delete(cacheKey)
    })
    return () => { cancelled = true }
  }, [groupKey, level, paths.length, visible])

  return (
    <div className="group-thumb" ref={hostRef}>
      {paths.length > 0 ? (
        <div className={`group-thumb-grid count-${Math.min(paths.length, 4)}`}>
          {paths.map((path, index) => (
            <img key={`${path}-${index}`} src={assetUrl(path)} alt="" draggable={false} loading="lazy" decoding="async" />
          ))}
        </div>
      ) : <div className="thumb-placeholder" />}
    </div>
  )
}

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
  const [mergeMode, setMergeMode] = useState(false)
  const [selectedKeys, setSelectedKeys] = useState<Set<string>>(new Set())
  const [mergeBusy, setMergeBusy] = useState(false)
  const [mergeMsg, setMergeMsg] = useState<string | null>(null)
  /** Groups awaiting the confirm dialog before the merge actually runs. */
  const [confirmGroups, setConfirmGroups] = useState<GroupInfo[] | null>(null)
  const [loadingGroups, setLoadingGroups] = useState(groups.length === 0)
  const [groupError, setGroupError] = useState('')
  const groupRequestRef = useRef(0)

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
  }, [])


  function onLevelChange(level: Granularity) {
    setGranularity(level)
    if (level !== 2) exitMergeMode()
    void loadGroups(currentSourceId, level)
  }

  function exitMergeMode() {
    setMergeMode(false)
    setSelectedKeys(new Set())
    setMergeMsg(null)
    setConfirmGroups(null)
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
    // Never merge from the "所有源" view: group keys are shared across
    // sources, so an unscoped merge would sweep in images the user never
    // selected. Ask for confirmation with the selected groups' thumbnails.
    if (currentSourceId == null) {
      setMergeMsg('请先在顶部选择具体来源目录，再执行合并（避免误并其他来源的图片）')
      return
    }
    setConfirmGroups(groups.filter((g) => selectedKeys.has(g.group_key)))
  }

  async function doMergeConfirmed() {
    const keys = Array.from(selectedKeys)
    if (keys.length < 2 || mergeBusy || currentSourceId == null) return
    setConfirmGroups(null)
    setMergeBusy(true)
    setMergeMsg(null)
    try {
      const r = await mergeGroups(2, keys, currentSourceId)
      setMergeMsg(`已合并 ${r.moved} 张到同一分组（自动重聚类不会再拆开，可在文件夹视角撤销合并）`)
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
      return g.workflow_name || '未命名 Model偏差'
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
            disabled={mergeBusy || currentSourceId == null}
            onClick={() => (mergeMode ? exitMergeMode() : setMergeMode(true))}
            title={currentSourceId == null
              ? '请先选择具体来源目录再合并（避免误并其他来源的图片）'
              : '勾选两个及以上 Prompt偏差 组，合并为同一组（自动重聚类不会拆开，可在文件夹视角撤销）'}
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
            <LazyGroupThumb groupKey={g.group_key} level={granularity} />
            <span className="group-count-badge">{g.count}</span>
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
      <AnimatePresence>
        {confirmGroups && confirmGroups.length > 0 && (
          <motion.div
            className="merge-confirm-overlay"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={() => setConfirmGroups(null)}
          >
            <motion.div
              className="merge-confirm"
              role="dialog"
              aria-modal="true"
              aria-label="确认合并分组"
              initial={{ opacity: 0, scale: 0.92, y: 10 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={{ opacity: 0, scale: 0.94, y: 6 }}
              transition={{ type: 'spring', stiffness: 320, damping: 28 }}
              onClick={(e) => e.stopPropagation()}
            >
              <h3>确认合并这 {confirmGroups.length} 个组？</h3>
              <p className="muted">合并后 {confirmGroups.reduce((n, g) => n + g.count, 0)} 张图片进入同一分组，自动重聚类不会再拆开；合并后可在该组的文件夹视角撤销。</p>
              <div className="merge-confirm-groups">
                {confirmGroups.map((g, i) => (
                  <motion.div
                    key={g.group_key}
                    className="merge-confirm-group"
                    initial={{ opacity: 0, y: 14 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ delay: 0.05 * i, duration: 0.18 }}
                  >
                    <LazyGroupThumb groupKey={g.group_key} level={granularity} />
                    <span className="group-count-badge">{g.count}</span>
                    <div className="group-prompt">{displayLabel(g)}</div>
                  </motion.div>
                ))}
              </div>
              <div className="row">
                <button type="button" className="ghost" disabled={mergeBusy} onClick={() => setConfirmGroups(null)}>取消</button>
                <button type="button" disabled={mergeBusy} onClick={() => void doMergeConfirmed()}>确认合并</button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}
