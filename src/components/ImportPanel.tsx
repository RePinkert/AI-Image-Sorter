import { useEffect, useRef, useState } from 'react'
import {
  addSourceAndScan,
  errorMessage,
  findComfySources,
  findWorkflowTemplates,
  listGroups,
  listSources,
  pickFolder,
} from '../api'
import type { FoundSourceDto, ScanResult, SourceRow, WorkflowTemplateDto } from '../types'
import { useStore } from '../store'
import { track } from '../telemetry'

export function ImportPanel() {
  const setView = useStore((s) => s.setView)
  const setSources = useStore((s) => s.setSources)
  const setGroups = useStore((s) => s.setGroups)
  const setCurrentSourceId = useStore((s) => s.setCurrentSourceId)
  const sources = useStore((s) => s.sources)
  const dataRevision = useStore((s) => s.dataRevision)
  const [found, setFound] = useState<FoundSourceDto[]>([])
  const [templates, setTemplates] = useState<WorkflowTemplateDto[]>([])
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState('')
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState('')
  const requestRef = useRef(0)

  useEffect(() => {
    const request = ++requestRef.current
    setLoading(true)
    setLoadError('')
    void Promise.allSettled([findComfySources(), findWorkflowTemplates(), listSources()]).then((results) => {
      if (request !== requestRef.current) return
      const [foundResult, templateResult, sourceResult] = results
      if (foundResult.status === 'fulfilled') setFound(foundResult.value)
      if (templateResult.status === 'fulfilled') setTemplates(templateResult.value)
      if (sourceResult.status === 'fulfilled') setSources(sourceResult.value)
      const failed = results.filter((result) => result.status === 'rejected')
      if (failed.length > 0) setLoadError(`${failed.length} 项本地数据加载失败，可稍后重试。`)
      setLoading(false)
    })
    return () => {
      requestRef.current += 1
    }
  }, [dataRevision, setSources])

  async function scan(path: string, kind: string) {
    setBusy(true)
    setMsg(`扫描中: ${path}`)
    const startedMs = Date.now()
    try {
      const res: ScanResult = await addSourceAndScan(path, kind)
       setMsg(`完成: 扫描 ${res.scanned} 张图，分 ${res.groups} 组${res.parse_errors > 0 ? `，解析失败 ${res.parse_errors} 张` : ''}`)
      const srcs = await listSources()
      setSources(srcs)
      setCurrentSourceId(res.source_id)
      const groups = await listGroups(res.source_id, 3)
      setGroups(groups)
      if (groups.length > 0) setView('groups')
      track('sync', {
        success: true,
        source_count: 1,
        duration_ms: Date.now() - startedMs,
      })
    } catch (e) {
      setMsg(`错误: ${errorMessage(e)}`)
      track('sync', { success: false, source_count: 1, duration_ms: Date.now() - startedMs })
    } finally {
      setBusy(false)
    }
  }

  async function manualPick() {
    try {
      const folder = await pickFolder()
      if (folder) await scan(folder, 'local')
    } catch (error) {
      setMsg(`选择目录失败：${errorMessage(error)}`)
    }
  }

  async function openSource(src: SourceRow) {
    setBusy(true)
    setMsg('')
    try {
      setCurrentSourceId(src.id)
      const groups = await listGroups(src.id, 3)
      setGroups(groups)
      setView('groups')
    } catch (error) {
      setMsg(`打开数据源失败：${errorMessage(error)}`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="panel">
      <h2>导入 ComfyUI 输出文件夹</h2>
      {loading && <p className="muted" role="status">正在读取本地数据…</p>}
      {loadError && <p className="action-error" role="alert">{loadError}</p>}
      <p className="hint">自动检索到的 ComfyUI 输出目录：</p>
      {found.length === 0 && <p className="muted">未找到，请手动选择。</p>}
      <ul className="source-list">
        {found.map((s) => (
          <li key={s.path}>
            <span className="path">{s.path}</span>
            <span className="origin">{s.origin}</span>
            <button type="button" disabled={busy} onClick={() => void scan(s.path, s.kind)}>
              扫描
            </button>
          </li>
        ))}
      </ul>

      <div className="row">
        <button type="button" disabled={busy} onClick={() => void manualPick()}>
          手动选择文件夹
        </button>
      </div>

      <h3 style={{ marginTop: 16 }}>已保存的 ComfyUI Workflow</h3>
      {templates.length === 0 && <p className="muted">未找到已保存 workflow。</p>}
      <ul className="source-list">
        {templates.map((template) => (
          <li key={template.path}>
            <span className="path">{template.name}</span>
            <span className="origin">{template.node_count} 节点 · {template.diffusion_models.join(', ') || '模型未识别'}</span>
            <span className="origin" title={template.path}>{template.topology_signature}</span>
          </li>
        ))}
      </ul>

      <h3 style={{ marginTop: 16 }}>已注册的源</h3>
      {sources.length === 0 && <p className="muted">暂无已注册的源。</p>}
      <ul className="source-list">
        {sources.map((s) => (
          <li key={s.id}>
            <span className="path">{s.path}</span>
            <span className="origin">{s.kind}</span>
            <button type="button" disabled={busy} onClick={() => void openSource(s)}>
              查看分组
            </button>
            <button type="button" disabled={busy} onClick={() => void scan(s.path, s.kind)}>
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
