import { useEffect, useState } from 'react'
import { deleteLabel, listLabels, upsertLabel } from '../api'
import type { LabelRow } from '../types'
import { useStore } from '../store'

const GESTURES = ['left', 'right', 'up', 'down']

export function Settings() {
  const setView = useStore((s) => s.setView)
  const setLabels = useStore((s) => s.setLabels)
  const [rows, setRows] = useState<LabelRow[]>([])
  const [draft, setDraft] = useState({ name: '', gesture: 'left', color: '#888888' })

  async function refresh() {
    const l = await listLabels()
    setRows(l)
    setLabels(l)
  }

  useEffect(() => {
    refresh()
  }, [])

  async function add() {
    if (!draft.name.trim()) return
    await upsertLabel(null, draft.name, draft.gesture, draft.color)
    setDraft({ ...draft, name: '' })
    refresh()
  }

  async function remove(id: number) {
    await deleteLabel(id)
    refresh()
  }

  return (
    <div className="panel">
      <h2>自定义标签 / 手势映射</h2>
      <p className="hint">每个手势映射到一个标签。滑动该方向时，图片会被打上对应标签并更新评分。</p>
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
                <button onClick={() => remove(r.id)}>删除</button>
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
        <button onClick={add}>添加</button>
      </div>

      <div className="row">
        <button onClick={() => setView('groups')}>返回</button>
        <ExportButtons />
      </div>
    </div>
  )
}

function ExportButtons() {
  const currentSourceId = useStore((s) => s.currentSourceId)
  const [busy, setBusy] = useState(false)
  return (
    <>
      <button
        disabled={busy}
        onClick={async () => {
          setBusy(true)
          const { exportData, pickSavePath } = await import('../api')
          const dest = await pickSavePath('export.csv')
          if (dest) {
            const n = await exportData(currentSourceId, 'csv', dest)
            alert(`已导出 ${n} 条到 ${dest}`)
          }
          setBusy(false)
        }}
      >
        导出 CSV
      </button>
      <button
        disabled={busy}
        onClick={async () => {
          setBusy(true)
          const { exportData, pickSavePath } = await import('../api')
          const dest = await pickSavePath('export.json')
          if (dest) {
            const n = await exportData(currentSourceId, 'json', dest)
            alert(`已导出 ${n} 条到 ${dest}`)
          }
          setBusy(false)
        }}
      >
        导出 JSON
      </button>
      <ArchiveButton />
    </>
  )
}

function ArchiveButton() {
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const granularity = useStore((s) => s.granularity)
  const [busy, setBusy] = useState(false)
  return (
    <button
      disabled={busy}
      onClick={async () => {
        setBusy(true)
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
        setBusy(false)
      }}
    >
      归档高分图（复制）
    </button>
  )
}
