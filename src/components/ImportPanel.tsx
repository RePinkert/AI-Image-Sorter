import { useCallback, useEffect, useRef, useState } from 'react'
import {
  addSourceAndScan,
  discardUnconfirmedImage,
  errorMessage,
  findComfySources,
  findWorkflowTemplates,
  listGroups,
  listSources,
  listUnconfirmedImages,
  pickFolder,
  refreshWorkflowTemplates,
} from '../api'
import type { FoundSourceDto, ScanResult, UnconfirmedImage, WorkflowTemplateDto } from '../types'
import { useStore } from '../store'
import { track } from '../telemetry'

/**
 * 数据源页（第 28 轮的信息架构重做）。
 *
 * ## 这一页到底在做什么
 *
 * 它曾经叫「导入 ComfyUI 输出文件夹」，但它实际承担**两件事**：
 *
 * 1. **进入工作台** —— 从已注册的源里挑一个，进分组列表（这是主任务）
 * 2. **加料** —— 扫描一个新的 ComfyUI 输出目录（这是维护动作）
 *
 * 旧版把两件事**平铺在同一行的两个同形按钮**里（「查看分组」/「重新扫描」），
 * 没有 class、没有分级 —— 于是「点哪个能进主页面」这件事，
 * 只有天天用的人靠肌肉记忆知道。这正是军规 06（按钮按主次分级）
 * 与军规 04（弱化凸显）要治的病。
 *
 * ## 现在的结构
 *
 * 1. **已注册的源**（主任务）：行可选中，选中后出现**全页唯一**的主操作
 *    「打开分组列表」。扫描类动作降级为辅助级。
 * 2. **添加来源**（次任务）：自动检索到的目录 + 手动选择。
 * 3. **已保存的 Workflow**（最低频）：纯信息，默认折叠。
 */
export function ImportPanel() {
  const setView = useStore((s) => s.setView)
  const setSources = useStore((s) => s.setSources)
  const setGroups = useStore((s) => s.setGroups)
  const setCurrentSourceId = useStore((s) => s.setCurrentSourceId)
  const sources = useStore((s) => s.sources)
  const currentSourceId = useStore((s) => s.currentSourceId)
  const dataRevision = useStore((s) => s.dataRevision)
  const [found, setFound] = useState<FoundSourceDto[]>([])
  const [templates, setTemplates] = useState<WorkflowTemplateDto[]>([])
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState('')
  const [msgKind, setMsgKind] = useState<'info' | 'error'>('info')
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState('')
  const [showWorkflows, setShowWorkflows] = useState(false)
  /** 用户挑中的源。主操作「打开分组列表」作用于它。 */
  const [pickedSourceId, setPickedSourceId] = useState<number | null>(null)
  /**
   * 身份无法确认、被保留待确认的旧记录（第 29 轮）。
   *
   * 这些记录带用户状态（评分 / 标签 / 手动分组）—— 宁可留着让用户决定，
   * 也不在一次改名后凭空消失。
   */
  const [unconfirmed, setUnconfirmed] = useState<UnconfirmedImage[]>([])
  const requestRef = useRef(0)

  const refreshUnconfirmed = useCallback(async () => {
    try {
      setUnconfirmed(await listUnconfirmedImages())
    } catch {
      setUnconfirmed([])
    }
  }, [])

  useEffect(() => {
    void refreshUnconfirmed()
  }, [dataRevision, refreshUnconfirmed])

  useEffect(() => {
    const request = ++requestRef.current
    setLoading(true)
    setLoadError('')
    void Promise.allSettled([findComfySources(), findWorkflowTemplates(), listSources()]).then((results) => {
      if (request !== requestRef.current) return
      const [foundResult, templateResult, sourceResult] = results
      if (foundResult.status === 'fulfilled') setFound(foundResult.value)
      if (templateResult.status === 'fulfilled') setTemplates(templateResult.value)
      if (sourceResult.status === 'fulfilled') {
        const rows = sourceResult.value
        setSources(rows)
        // 默认挑中上次用的那个源 —— 老用户回来就是「一步到位」：
        // 主按钮已经指向他上次工作的源，直接按「打开分组列表」即可。
        setPickedSourceId((prev) => {
          if (prev != null && rows.some((r) => r.id === prev)) return prev
          const remembered = currentSourceId
          if (remembered != null && rows.some((r) => r.id === remembered)) return remembered
          return rows.length === 1 ? rows[0].id : null
        })
      }
      const failed = results.filter((result) => result.status === 'rejected')
      if (failed.length > 0) setLoadError(`${failed.length} 项本地数据加载失败，可稍后重试。`)
      setLoading(false)
    })
    return () => {
      requestRef.current += 1
    }
  }, [dataRevision, setSources, currentSourceId])

  const picked = sources.find((s) => s.id === pickedSourceId) ?? null

  async function scan(path: string, kind: string) {
    setBusy(true)
    setMsg(`正在扫描 ${path}`)
    setMsgKind('info')
    const startedMs = Date.now()
    try {
      const res: ScanResult = await addSourceAndScan(path, kind)
      const srcs = await listSources()
      setSources(srcs)
      setCurrentSourceId(res.source_id)
      setPickedSourceId(res.source_id)
      const groups = await listGroups(res.source_id, 3)
      setGroups(groups)
      const failed = res.parse_errors > 0 ? `，其中 ${res.parse_errors} 张解析失败` : ''
      // 改名识别：路径变了但**行没换** → 评分 / 标签 / 手动分组全保住。
      // 这件事必须**说出来** —— 用户有权知道库是怎么变的。
      const renamed = res.renamed > 0 ? `，识别出 ${res.renamed} 个文件被改名并已按内容更新记录` : ''
      const unconfirmed = res.unconfirmed > 0 ? `，${res.unconfirmed} 个记录无法确认身份已保留待确认` : ''
      // 扫描完**不自动跳走**：扫描是「备料」，进入工作台是「打开」。
      // 两条路各自的按钮只有一条，模型才清楚。
      setMsg(`已扫描 ${res.scanned} 张图，分成 ${res.groups} 组${renamed}${unconfirmed}${failed}。按「打开分组列表」继续。`)
      setMsgKind('info')
      void refreshUnconfirmed()
      track('sync', {
        success: true,
        source_count: 1,
        duration_ms: Date.now() - startedMs,
      })
    } catch (e) {
      setMsg(`扫描失败：${errorMessage(e)}`)
      setMsgKind('error')
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
      setMsgKind('error')
    }
  }

  async function openPickedSource() {
    if (picked == null || busy) return
    setBusy(true)
    try {
      setCurrentSourceId(picked.id)
      const groups = await listGroups(picked.id, 3)
      setGroups(groups)
      setView('groups')
    } catch (e) {
      setMsg(`打开数据源失败：${errorMessage(e)}`)
      setMsgKind('error')
    } finally {
      setBusy(false)
    }
  }

  const hasRegistered = sources.length > 0

  return (
    <div className="panel" data-view="import">
      <div className="panel-head">
        <h2>数据源</h2>
        {hasRegistered && (
          <button type="button" className="btn-tertiary" onClick={() => setView('groups')}>
            返回分组列表
          </button>
        )}
      </div>

      {/* ---- 区块 1 · 已注册的源（主任务） ---- */}
      <section className="import-section">
        <h3 className="import-section-title">已注册的源</h3>
        {loading && <p className="muted" role="status">正在读取本地数据…</p>}
        {loadError && <p className="action-error" role="alert">{loadError}</p>}
        {!loading && !hasRegistered && (
          <div className="import-empty">
            <p className="muted">还没有任何数据源。先在下面「添加来源」里扫描一个 ComfyUI 输出目录。</p>
            {/* 军规 20：空状态必须给出路 —— 告诉用户「什么都没有」之后，
                得有一个按钮能让他往前走一步。 */}
            <button type="button" className="btn-secondary" disabled={busy} onClick={() => void manualPick()}>
              手动选择文件夹
            </button>
          </div>
        )}
        {hasRegistered && (
          <>
            <ul className="source-list source-select-list">
              {sources.map((s) => {
                const pickedHere = s.id === pickedSourceId
                return (
                  <li key={s.id}>
                    <button
                      type="button"
                      className={`source-item${pickedHere ? ' selected' : ''}`}
                      data-source-id={s.id}
                      aria-pressed={pickedHere}
                      disabled={busy}
                      onClick={() => setPickedSourceId(s.id)}
                    >
                      <span className="path">{s.alias ?? s.path}</span>
                      <span className="origin">{s.kind}</span>
                    </button>
                    <button
                      type="button"
                      className="btn-tertiary"
                      disabled={busy}
                      title={`重新扫描 ${s.alias ?? s.path}`}
                      onClick={() => void scan(s.path, s.kind)}
                    >
                      重新扫描
                    </button>
                  </li>
                )
              })}
            </ul>
            <div className="row">
              <button
                type="button"
                className="btn-primary"
                data-nav="open-groups"
                disabled={busy || picked == null}
                onClick={() => void openPickedSource()}
                title={picked ? `打开「${picked.alias ?? picked.path}」的分组列表` : '先选一个源'}
              >
                打开分组列表
              </button>
              {picked == null && <span className="muted">先选一个源</span>}
            </div>
            {unconfirmed.length > 0 && (
              <div className="import-note" role="status">
                <p>
                  有 {unconfirmed.length} 条记录的文件已不在原路径，且无法按内容确认身份 ——
                  记录已保留（评分、标签、手动分组都没丢）。确认文件确实被删了，再清理掉。
                </p>
                <ul className="unconfirmed-list">
                  {unconfirmed.map((row) => (
                    <li key={row.id}>
                      <span className="path" title={row.abs_path}>{row.filename}</span>
                      <button
                        type="button"
                        className="btn-tertiary"
                        onClick={async () => {
                          await discardUnconfirmedImage(row.id)
                          await refreshUnconfirmed()
                        }}
                      >
                        确认已删除
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </>
        )}
      </section>

      {/* ---- 区块 2 · 添加来源（次任务） ---- */}
      <section className="import-section">
        <h3 className="import-section-title">添加来源</h3>
        {found.length === 0 ? (
          <div className="import-empty">
            <p className="muted">没有自动检索到 ComfyUI 输出目录。</p>
            <button type="button" className="btn-secondary" disabled={busy} onClick={() => void manualPick()}>
              手动选择文件夹
            </button>
          </div>
        ) : (
          <>
            <p className="hint">自动检索到的 ComfyUI 输出目录：</p>
            <ul className="source-list">
              {found.map((s) => (
                <li key={s.path}>
                  <span className="path">{s.path}</span>
                  <span className="origin">{s.origin}</span>
                  <button
                    type="button"
                    className="btn-secondary"
                    disabled={busy}
                    onClick={() => void scan(s.path, s.kind)}
                  >
                    扫描
                  </button>
                </li>
              ))}
            </ul>
            <div className="row">
              <button type="button" className="btn-secondary" disabled={busy} onClick={() => void manualPick()}>
                手动选择文件夹
              </button>
            </div>
          </>
        )}
      </section>

      {/* ---- 区块 3 · 已保存的 Workflow（最低频，纯信息） ---- */}
      <section className="import-section">
        <button
          type="button"
          className="import-section-toggle"
          aria-expanded={showWorkflows}
          onClick={() => setShowWorkflows(!showWorkflows)}
        >
          {showWorkflows ? '收起' : '展开'}已保存的 ComfyUI Workflow（{templates.length}）
        </button>
        {showWorkflows && (
          <>
            {templates.length === 0 ? (
              <div className="import-empty">
                <p className="muted">
                  没有找到已保存的 workflow。workflow 是 ComfyUI 里的工作流模板 ——
                  它带着模型与节点信息，扫描输出目录时用来识别每张图是哪个模型生成的。
                </p>
                {/* 空状态也要给出路：让用户能就地重新检索一次。 */}
                <button
                  type="button"
                  className="btn-secondary"
                  disabled={busy}
                  onClick={async () => {
                    setBusy(true)
                    try {
                      await refreshWorkflowTemplates()
                      setTemplates(await findWorkflowTemplates())
                    } catch (e) {
                      setMsg(`重新检索 workflow 失败：${errorMessage(e)}`)
                      setMsgKind('error')
                    } finally {
                      setBusy(false)
                    }
                  }}
                >
                  重新检索
                </button>
              </div>
            ) : (
              <ul className="source-list">
                {templates.map((template) => (
                  <li key={template.path}>
                    <span className="path">{template.name}</span>
                    <span className="origin">
                      {template.node_count} 节点 · {(template.diffusion_models ?? []).join(', ') || '模型未识别'}
                    </span>
                    <span className="origin" title={template.path}>{template.topology_signature}</span>
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </section>

      {msg && (
        <p className={msgKind === 'error' ? 'action-error' : 'sync-warn'} role="status">
          {msg}
        </p>
      )}
      {busy && <p className="muted" role="status">处理中…</p>}
    </div>
  )
}
