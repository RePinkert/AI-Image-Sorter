import { useEffect, useState } from 'react'
import {
  addSourceAndScan,
  findComfySources,
  listGroups,
  listSources,
  pickFolder,
} from '../api'
import type { FoundSourceDto, ScanResult, SourceRow } from '../types'
import { useStore } from '../store'

export function ImportPanel() {
  const setView = useStore((s) => s.setView)
  const setSources = useStore((s) => s.setSources)
  const setGroups = useStore((s) => s.setGroups)
  const setCurrentSourceId = useStore((s) => s.setCurrentSourceId)
  const sources = useStore((s) => s.sources)
  const [found, setFound] = useState<FoundSourceDto[]>([])
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState('')

  useEffect(() => {
    findComfySources().then(setFound).catch(() => {})
    listSources().then(setSources).catch(() => {})
  }, [setSources])

  async function scan(path: string, kind: string) {
    setBusy(true)
    setMsg(`扫描中: ${path}`)
    try {
      const res: ScanResult = await addSourceAndScan(path, kind)
      setMsg(`完成: 扫描 ${res.scanned} 张图，分 ${res.groups} 组`)
      const srcs = await listSources()
      setSources(srcs)
      setCurrentSourceId(res.source_id)
      const groups = await listGroups(res.source_id, 3)
      setGroups(groups)
      if (groups.length > 0) setView('groups')
    } catch (e) {
      setMsg(`错误: ${e}`)
    } finally {
      setBusy(false)
    }
  }

  async function manualPick() {
    const folder = await pickFolder()
    if (folder) await scan(folder, 'local')
  }

  async function openSource(src: SourceRow) {
    setCurrentSourceId(src.id)
    const groups = await listGroups(src.id, 3)
    setGroups(groups)
    setView('groups')
  }

  return (
    <div className="panel">
      <h2>导入 ComfyUI 输出文件夹</h2>
      <p className="hint">自动检索到的 ComfyUI 输出目录：</p>
      {found.length === 0 && <p className="muted">未找到，请手动选择。</p>}
      <ul className="source-list">
        {found.map((s) => (
          <li key={s.path}>
            <span className="path">{s.path}</span>
            <span className="origin">{s.origin}</span>
            <button disabled={busy} onClick={() => scan(s.path, s.kind)}>
              扫描
            </button>
          </li>
        ))}
      </ul>

      <div className="row">
        <button disabled={busy} onClick={manualPick}>
          手动选择文件夹
        </button>
      </div>

      <h3 style={{ marginTop: 16 }}>已注册的源</h3>
      {sources.length === 0 && <p className="muted">暂无已注册的源。</p>}
      <ul className="source-list">
        {sources.map((s) => (
          <li key={s.id}>
            <span className="path">{s.path}</span>
            <span className="origin">{s.kind}</span>
            <button disabled={busy} onClick={() => openSource(s)}>
              查看分组
            </button>
            <button disabled={busy} onClick={() => scan(s.path, s.kind)}>
              重新扫描
            </button>
          </li>
        ))}
      </ul>

      {msg && <p className="msg">{msg}</p>}
      {busy && <p className="muted">处理中…</p>}
    </div>
  )
}
