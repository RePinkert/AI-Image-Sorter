import { useEffect, useState } from 'react'
import { assetUrl, getGroupThumbnails, listGroups, listSources } from '../api'
import type { Granularity, GroupInfo, GroupThumbDto, SourceRow } from '../types'
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
  const granularity = useStore((s) => s.granularity)
  const setGranularity = useStore((s) => s.setGranularity)
  const [sources, setSources] = useState<SourceRow[]>([])
  const [thumbs, setThumbs] = useState<Record<string, string>>({})
  const [loadingThumbs, setLoadingThumbs] = useState(false)

  useEffect(() => {
    listSources().then(setSources).catch(() => {})
  }, [])

  async function loadGroups(sourceId: number | null, level: Granularity) {
    setCurrentSourceId(sourceId)
    const g = await listGroups(sourceId ?? undefined, level)
    setGroups(g)
    setThumbs({})
  }

  useEffect(() => {
    if (groups.length === 0 && sources.length > 0) {
      loadGroups(sources[0].id, granularity)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sources])

  useEffect(() => {
    if (groups.length > 0) {
      loadThumbs(groups)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [groups])

  async function loadThumbs(gs: GroupInfo[]) {
    if (gs.length === 0) return
    setLoadingThumbs(true)
    const keys = gs.map((g) => g.group_key)
    try {
      const dtos: GroupThumbDto[] = await getGroupThumbnails(keys, granularity)
      const map: Record<string, string> = {}
      dtos.forEach((d) => {
        map[d.group_key] = d.thumb_path
      })
      setThumbs(map)
    } catch {
      // ignore
    } finally {
      setLoadingThumbs(false)
    }
  }

  function onLevelChange(level: Granularity) {
    setGranularity(level)
    loadGroups(currentSourceId, level)
  }

  function openGroup(g: GroupInfo) {
    setCurrentGroupKey(g.group_key)
    setView('swipe')
  }

  function displayLabel(g: GroupInfo): string {
    if (granularity === 0) {
      return g.source_path || '(文件夹)'
    }
    return g.prompt_pos.slice(0, 120) || '(无 prompt)'
  }

  return (
    <div className="panel">
      <h2>分组列表</h2>
      <div className="row">
        <select
          value={currentSourceId ?? ''}
          onChange={(e) => loadGroups(e.target.value ? Number(e.target.value) : null, granularity)}
        >
          <option value="">所有源</option>
          {sources.map((s) => (
            <option key={s.id} value={s.id}>
              {s.alias ?? s.path}
            </option>
          ))}
        </select>
        <button onClick={() => setView('import')}>+ 导入新源</button>
        <button onClick={() => setView('settings')}>标签设置 / 导出</button>
      </div>

      <div className="granularity-bar">
        {LEVELS.map((l) => (
          <button
            key={l.value}
            className={granularity === l.value ? 'gran-active' : ''}
            onClick={() => onLevelChange(l.value)}
          >
            {l.label}
          </button>
        ))}
      </div>

      {groups.length === 0 && <p className="muted">暂无分组，请先导入。</p>}
      {loadingThumbs && groups.length > 0 && (
        <p className="muted hint">生成缩略图中…</p>
      )}
      <div className="group-grid">
        {groups.map((g) => (
          <div
            key={g.group_key}
            className="group-card"
            onClick={() => openGroup(g)}
            title={displayLabel(g)}
          >
            <div className="group-thumb">
              {thumbs[g.group_key] ? (
                <img src={assetUrl(thumbs[g.group_key])} alt="" draggable={false} />
              ) : (
                <div className="thumb-placeholder" />
              )}
              <span className="group-count-badge">{g.count}</span>
            </div>
            <div className="group-prompt">{displayLabel(g)}</div>
            <div className="group-meta">
              {g.checkpoint || '—'} · {g.source_kind}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
