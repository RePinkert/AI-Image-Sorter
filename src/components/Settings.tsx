import { useEffect, useState } from 'react'
import { deleteLabel, errorMessage, listLabels, upsertLabel } from '../api'
import type { LabelRow } from '../types'
import { useStore } from '../store'
import { KeymapPanel } from './KeymapPanel'
import { exportDiagnostics, isTelemetryEnabled, setTelemetryEnabled } from '../telemetry'

const GESTURES = ['left', 'right', 'up', 'down']

function ClusterSection() {
  const currentSourceId = useStore((s) => s.currentSourceId)
  const l2Threshold = useStore((s) => s.l2Threshold)
  const setL2Threshold = useStore((s) => s.setL2Threshold)
  const setGroups = useStore((s) => s.setGroups)
  const granularity = useStore((s) => s.granularity)
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<string | null>(null)

  async function apply(threshold: number) {
    if (currentSourceId == null) {
      setMsg('请先在分组列表选择一个数据源')
      return
    }
    setBusy(true)
    setMsg(null)
    try {
      const { reclusterSource } = await import('../api')
      await reclusterSource(currentSourceId, threshold)
      const { listGroups } = await import('../api')
      const g = await listGroups(currentSourceId, granularity)
      setGroups(g)
      setMsg(`已按阈值 ${threshold.toFixed(2)} 重新聚类 L2 组`)
    } catch (e) {
      setMsg(`重新聚类失败：${errorMessage(e)}`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="msg">
      <h3 style={{ marginTop: 0 }}>Prompt 相似度阈值（L2 聚类）</h3>
      <p className="hint">
        Jaccard 阈值：AI prompt 通常共享长前缀（质量标签、艺术家名等）而仅在尾部短句上有差异。
        因此即使视觉输出明显不同，文本 Jaccard 也常高达 0.8+。
        推荐范围 <strong>0.25 ~ 0.35</strong> 以区分基于 prompt 小改动获得的不同输出。
        值越低，聚合越松；值越高，要求 prompt 文本越接近才归入同一组。
      </p>
      <div className="row">
        <input
          type="range"
          min={0.2}
          max={0.8}
          step={0.05}
          value={l2Threshold}
          disabled={busy}
          onChange={(e) => setL2Threshold(Number(e.target.value))}
        />
        <span style={{ minWidth: 50, color: 'var(--accent)', fontWeight: 700 }}>
          {l2Threshold.toFixed(2)}
        </span>
        <button type="button" disabled={busy} onClick={() => void apply(l2Threshold)}>
          应用并重新聚类
        </button>
      </div>
      {msg && <p className="hint">{msg}</p>}
    </div>
  )
}

export function Settings() {
  const setView = useStore((s) => s.setView)
  const setLabels = useStore((s) => s.setLabels)
  const [rows, setRows] = useState<LabelRow[]>([])
  const [draft, setDraft] = useState({ name: '', gesture: 'left', color: '#888888' })
  const [showKeymap, setShowKeymap] = useState(false)
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [telemetryEnabled, setTelemetryEnabledState] = useState(isTelemetryEnabled)

  async function refresh() {
    setLoading(true)
    setError('')
    try {
      const l = await listLabels()
      setRows(l)
      setLabels(l)
    } catch (cause) {
      setError(errorMessage(cause))
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void refresh()
  }, [])

  async function add() {
    if (!draft.name.trim()) return
    setBusy(true)
    setError('')
    try {
      await upsertLabel(null, draft.name, draft.gesture, draft.color)
      setDraft({ ...draft, name: '' })
      await refresh()
    } catch (cause) {
      setError(errorMessage(cause))
    } finally {
      setBusy(false)
    }
  }

  async function remove(id: number) {
    setBusy(true)
    setError('')
    try {
      await deleteLabel(id)
      await refresh()
    } catch (cause) {
      setError(errorMessage(cause))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="panel">
      <h2>自定义标签 / 手势映射</h2>
      <p className="hint">每个手势映射到一个标签。滑动该方向时，图片会被打上对应标签并更新评分。</p>
      {loading && <p className="muted" role="status">正在加载标签…</p>}
      {error && <p className="action-error" role="alert">{error}</p>}
      {!loading && !error && rows.length === 0 && <p className="muted">暂无标签。</p>}
      <table className="label-table">
        <thead>
          <tr>
            <th>标签</th>
            <th>手势</th>
            <th>颜色</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.id}>
              <td>{r.name}</td>
              <td>{r.gesture}</td>
              <td>
                <span className="color-dot" style={{ background: r.color ?? '#888' }} />
              </td>
              <td>
                <button type="button" disabled={busy} onClick={() => void remove(r.id)}>删除</button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <div className="row">
        <input
          placeholder="标签名"
          value={draft.name}
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
        />
        <select
          value={draft.gesture}
          onChange={(e) => setDraft({ ...draft, gesture: e.target.value })}
        >
          {GESTURES.map((g) => (
            <option key={g} value={g}>
              {g}
            </option>
          ))}
        </select>
        <input
          type="color"
          value={draft.color}
          onChange={(e) => setDraft({ ...draft, color: e.target.value })}
        />
        <button type="button" disabled={busy || loading} onClick={() => void add()}>添加</button>
      </div>

      <div className="row">
        <button type="button" onClick={() => setView('groups')}>返回</button>
        <button type="button" className={showKeymap ? 'gran-active' : ''} onClick={() => setShowKeymap(!showKeymap)}>
          {showKeymap ? '收起键位设置' : '键位设置'}
        </button>
        <ExportButtons />
      </div>

      {showKeymap && <KeymapPanel />}

      <div className="msg settings-telemetry">
        <h3 style={{ marginTop: 0 }}>本地行为记录</h3>
        <label className="check">
          <input
            type="checkbox"
            checked={telemetryEnabled}
            onChange={(event) => {
              const enabled = event.target.checked
              setTelemetryEnabledState(enabled)
              setTelemetryEnabled(enabled)
            }}
          />
          启用本地事件记录
        </label>
        <button type="button" onClick={exportDiagnostics}>导出脱敏诊断</button>
      </div>

      <ClusterSection />
    </div>
  )
}

function ExportButtons() {
  const currentSourceId = useStore((s) => s.currentSourceId)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  async function run(format: 'csv' | 'json') {
    setBusy(true)
    setError('')
    try {
      const { exportData, pickSavePath } = await import('../api')
      const dest = await pickSavePath(`export.${format}`)
      if (!dest) return
      const n = await exportData(currentSourceId, format, dest)
      alert(`已导出 ${n} 条到 ${dest}`)
    } catch (cause) {
      setError(`导出失败：${errorMessage(cause)}`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <button
        type="button"
        disabled={busy}
        onClick={() => void run('csv')}
      >
        导出 CSV
      </button>
      <button
        type="button"
        disabled={busy}
        onClick={() => void run('json')}
      >
        导出 JSON
      </button>
      <ArchiveButton />
      {error && <span className="sync-error" role="alert">{error}</span>}
    </>
  )
}

function ArchiveButton() {
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  return (
    <>
      <button
        type="button"
        disabled={busy}
        onClick={async () => {
        setBusy(true)
        setError('')
        try {
          const { archiveCopy, pickFolder, listGroupImages } = await import('../api')
          const dest = await pickFolder()
          if (dest && currentGroupKey != null) {
            const imgs = await listGroupImages(currentGroupKey, granularity)
            const top = [...imgs]
              .filter((i) => i.score != null)
              .sort((a, b) => (b.score ?? 0) - (a.score ?? 0))
              .slice(0, Math.max(1, Math.ceil(imgs.length / 2)))
              .map((i) => i.id)
            if (top.length > 0) {
              const n = await archiveCopy(top, dest, 'label')
              alert(`已复制 ${n} 张高分图到 ${dest}`)
            }
          }
        } catch (cause) {
          setError(`归档失败：${errorMessage(cause)}`)
        } finally {
          setBusy(false)
        }
      }}
      >
        归档高分图（复制）
      </button>
      {error && <span className="sync-error" role="alert">{error}</span>}
    </>
  )
}
