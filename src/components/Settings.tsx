import { useCallback, useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { deleteLabel, errorMessage, isWebDev, listLabels, upsertLabel } from '../api'
import type {
  L2Mode,
  LabelRow,
  SignatureProgressDto,
  TextCoverageDto,
  TextStatusDto,
  VisionStatusDto,
} from '../types'
import { useStore } from '../store'
import { KeymapPanel } from './KeymapPanel'
import { exportDiagnostics, isTelemetryEnabled, setTelemetryEnabled } from '../telemetry'

const GESTURES = ['left', 'right', 'up', 'down']

function ClusterSection() {
  const currentSourceId = useStore((s) => s.currentSourceId)
  const l2Threshold = useStore((s) => s.l2Threshold)
  const setL2Threshold = useStore((s) => s.setL2Threshold)
  const l2Mode = useStore((s) => s.l2Mode)
  const setL2Mode = useStore((s) => s.setL2Mode)
  const l2VisualThreshold = useStore((s) => s.l2VisualThreshold)
  const setL2VisualThreshold = useStore((s) => s.setL2VisualThreshold)
  const l2SemanticThreshold = useStore((s) => s.l2SemanticThreshold)
  const setL2SemanticThreshold = useStore((s) => s.setL2SemanticThreshold)
  const setGroups = useStore((s) => s.setGroups)
  const granularity = useStore((s) => s.granularity)
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<string | null>(null)

  // 三种量纲：文本是 Jaccard，视觉是 CLIP 余弦，语义是 BGE 余弦。
  // 阈值**分开存** —— 0.75 是 CLIP 的值，绝不可沿用到 BGE（量纲不同）。
  const semantic = l2Mode === 'semantic'
  const cosine = l2Mode !== 'text'
  const threshold = semantic
    ? l2SemanticThreshold
    : cosine
      ? l2VisualThreshold
      : l2Threshold
  const min = cosine ? 0.6 : 0.2
  const max = cosine ? 0.95 : 0.8
  const step = cosine ? 0.01 : 0.05

  async function apply() {
    if (currentSourceId == null) {
      setMsg('请先在分组列表选择一个数据源')
      return
    }
    setBusy(true)
    setMsg(null)
    try {
      const { reclusterSource } = await import('../api')
      await reclusterSource(
        currentSourceId,
        l2Threshold,
        l2Mode,
        l2VisualThreshold,
        l2SemanticThreshold,
      )
      const { listGroups } = await import('../api')
      const g = await listGroups(currentSourceId, granularity)
      setGroups(g)
      setMsg(
        cosine
          ? `已按余弦阈值 ${threshold.toFixed(2)} 重新聚类 L2 组`
          : `已按阈值 ${threshold.toFixed(2)} 重新聚类 L2 组`,
      )
    } catch (e) {
      setMsg(`重新聚类失败：${errorMessage(e)}`)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="msg">
      <h3 style={{ marginTop: 0 }}>L2 聚类方式</h3>
      <p className="hint">
        <strong>文本</strong>：按 prompt 的词重合度分组（历史行为）。
        <strong>视觉</strong>：按图像的实际相似度分组 —— 实测在真实语料上，
        它与「视觉上真的像」的吻合度 96%，而文本只有 57%，并且追平了手工整理的结果。
        <strong>语义</strong>：按 prompt 的语义分组（不读图，快得多，但阈值尚未标定）。
      </p>
      <div className="row">
        <select
          value={l2Mode}
          disabled={busy}
          onChange={(e) => setL2Mode(e.target.value as L2Mode)}
          aria-label="L2 聚类方式"
        >
          <option value="text">文本（词重合）</option>
          <option value="visual">视觉（图像相似度）</option>
          <option value="semantic">语义（prompt 语义）</option>
        </select>
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={threshold}
          disabled={busy}
          onChange={(e) => {
            const value = Number(e.target.value)
            if (semantic) setL2SemanticThreshold(value)
            else if (cosine) setL2VisualThreshold(value)
            else setL2Threshold(value)
          }}
        />
        <span style={{ minWidth: 50, color: 'var(--accent)', fontWeight: 700 }}>
          {threshold.toFixed(2)}
        </span>
        <button className="btn-primary" type="button" disabled={busy} onClick={() => void apply()}>
          应用并重新聚类
        </button>
      </div>
      {semantic && (
        <p className="hint">
          <b>语义档的余弦阈值尚未标定。</b>0.75 是<b>视觉</b>档的值，两者的量纲不同，
          绝不可沿用。当前 0.70 是暂定值 —— 阶段七轮 3 会用「合成语料判别力 +
          你自己的分组反标定」给出建议值，仍需你按「应用并重新聚类」才生效。
        </p>
      )}
      {cosine && !semantic && (
        <p className="hint">
          余弦阈值：值越低聚合越松。推荐 <strong>0.75</strong> —— 实测这是与手工整理
          质量相当、又不会把明显不同的图粘在一起的位置。
        </p>
      )}
      {!cosine && (
        <p className="hint">
          Jaccard 阈值：AI prompt 通常共享长前缀（质量标签、艺术家名等）而仅在尾部短句上有差异。
          因此即使视觉输出明显不同，文本 Jaccard 也常高达 0.8+。
          推荐范围 <strong>0.25 ~ 0.35</strong>。
        </p>
      )}
      {msg && <p className="hint">{msg}</p>}
    </div>
  )
}

function SignatureSection() {
  const currentSourceId = useStore((s) => s.currentSourceId)
  const [status, setStatus] = useState<VisionStatusDto | null>(null)
  const [busy, setBusy] = useState(false)
  const [progress, setProgress] = useState<SignatureProgressDto | null>(null)
  const [msg, setMsg] = useState<string | null>(null)

  useEffect(() => {
    if (isWebDev()) return
    let alive = true
    void (async () => {
      try {
        const { visionStatus } = await import('../api')
        const s = await visionStatus()
        if (alive) setStatus(s)
      } catch {
        // 状态拿不到时保持「未知」，按钮禁用即可
      }
    })()
    return () => {
      alive = false
    }
  }, [])

  // 进度事件：批处理要跑几分钟，必须让用户看得见。
  useEffect(() => {
    if (isWebDev()) return
    let unlisten: (() => void) | undefined
    void (async () => {
      const stop = await listen<SignatureProgressDto>('signature-progress', (event) => {
        setProgress(event.payload)
      })
      unlisten = stop
    })()
    return () => unlisten?.()
  }, [])

  async function run() {
    if (currentSourceId == null) {
      setMsg('请先在分组列表选择一个数据源')
      return
    }
    setBusy(true)
    setMsg(null)
    setProgress(null)
    try {
      const { computeSignatures } = await import('../api')
      const r = await computeSignatures(currentSourceId)
      const parts = [`新算 ${r.computed} 张`]
      if (r.skipped > 0) parts.push(`已有 ${r.skipped} 张跳过`)
      if (r.failed > 0) parts.push(`${r.failed} 张失败`)
      if (r.cancelled) parts.push('已取消')
      setMsg(`${parts.join(' · ')}（${r.model_id} · ${r.dim} 维）`)
    } catch (e) {
      setMsg(`识图失败：${errorMessage(e)}`)
    } finally {
      setBusy(false)
      setProgress(null)
    }
  }

  async function cancel() {
    try {
      const { cancelSignatures } = await import('../api')
      await cancelSignatures()
    } catch {
      // 取消失败不打扰用户：批处理自己会结束
    }
  }

  const available = status?.available === true
  const percent = progress && progress.total > 0
    ? Math.round((progress.done / progress.total) * 100)
    : 0

  return (
    <div className="msg">
      <h3 style={{ marginTop: 0 }}>识图（视觉签名）</h3>
      <p className="hint">
        在本机跑视觉模型，给每张图算一个向量。向量用于语义聚类与「可能相关」推荐 ——
        全程离线，不上传任何图片。CPU 约 3 张/秒，1000 张需要几分钟；已算过的会跳过。
      </p>
      {!available && status && (
        <p className="hint">
          未找到视觉模型权重，功能不可用。请运行 <code>scripts\download_models.ps1</code> 后重启应用。
        </p>
      )}
      {!available && !status && (
        <p className="hint">识图在桌面端运行（本页在浏览器里只做预览）。</p>
      )}
      <div className="row">
        <button
          className="btn-secondary"
          type="button"
          disabled={busy || !available || currentSourceId == null}
          onClick={() => void run()}
        >
          {busy ? '识图中…' : '开始识图'}
        </button>
        {busy && (
          <button className="btn-tertiary" type="button" onClick={() => void cancel()}>
            取消
          </button>
        )}
        {progress && (
          <span className="muted">
            {progress.done} / {progress.total}（{percent}%）
          </span>
        )}
      </div>
      {msg && <p className="hint">{msg}</p>}
    </div>
  )
}

/**
 * 构建指纹（一行）。
 *
 * 为什么只有一行：能力清单是**诊断数据**，不是用户偏好 —— 整块表格塞进
 * 设置页会变成一坨开发者术语。完整清单（版本 / 构建时刻 / 提交 / 能力标记
 * 串 / 模型塔就位状态）在**「导出脱敏诊断」**的 JSON 里（`build` 段）。
 *
 * 这里留一行的唯一用途：让用户确认「我现在跑的这一份，就是刚审过的那一份」。
 * 拿不到（浏览器预览模式 / 老后端）就**什么都不显示** —— 静默降级，
 * 不给用户添一条错误提示。
 */
function BuildStamp() {
  const [line, setLine] = useState<string | null>(null)
  useEffect(() => {
    let alive = true
    void (async () => {
      try {
        const { appCapabilities } = await import('../api')
        const c = await appCapabilities()
        if (alive && c.version) {
          setLine(`版本 ${c.version} · 构建 ${c.build_ts}`)
        }
      } catch {
        // 拿不到就不显示（诊断导出里仍然带完整清单）。
      }
    })()
    return () => {
      alive = false
    }
  }, [])
  if (!line) return null
  return <p className="hint">{line}</p>
}

/**
 * 文本塔（prompt 语义）面板。
 *
 * 与「识图」面板的差别：文本塔**不读图**，输入是 prompt 文本 —— 所以快得多
 * （1000 条 prompt 约十几秒，视觉塔要 5 分钟）。
 *
 * 面板要回答三件事：塔在不在（权重 + 分词器都要在）、覆盖率多少、怎么补齐。
 */
function TextSignatureSection() {
  const currentSourceId = useStore((s) => s.currentSourceId)
  const [status, setStatus] = useState<TextStatusDto | null>(null)
  const [coverage, setCoverage] = useState<TextCoverageDto | null>(null)
  const [busy, setBusy] = useState(false)
  const [progress, setProgress] = useState<SignatureProgressDto | null>(null)
  const [msg, setMsg] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    if (currentSourceId == null) {
      setCoverage(null)
      return
    }
    try {
      const { textCoverage } = await import('../api')
      setCoverage(await textCoverage(currentSourceId))
    } catch {
      setCoverage(null)
    }
  }, [currentSourceId])

  useEffect(() => {
    if (isWebDev()) return
    let alive = true
    void (async () => {
      try {
        const { textStatus } = await import('../api')
        const s = await textStatus()
        if (alive) setStatus(s)
      } catch {
        // 拿不到就保持「未知」，按钮禁用即可。
      }
    })()
    return () => {
      alive = false
    }
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh])

  useEffect(() => {
    if (isWebDev()) return
    let alive = true
    let unlisten: (() => void) | undefined
    void (async () => {
      const stop = await listen<SignatureProgressDto>('text-signature-progress', (event) => {
        if (!alive) return
        setProgress(event.payload)
      })
      unlisten = stop
    })()
    return () => {
      alive = false
      unlisten?.()
    }
  }, [])

  async function run() {
    if (currentSourceId == null) {
      setMsg('请先在分组列表选择一个数据源')
      return
    }
    setBusy(true)
    setMsg(null)
    setProgress(null)
    try {
      const { computeTextSignatures } = await import('../api')
      const r = await computeTextSignatures(currentSourceId)
      const parts = [`已算 ${r.computed} 条`]
      if (r.skipped > 0) parts.push(`跳过 ${r.skipped} 条`)
      if (r.failed > 0) parts.push(`${r.failed} 条失败`)
      setMsg(`${parts.join(' · ')}`)
      await refresh()
    } catch (e) {
      setMsg(`文本签名失败：${errorMessage(e)}`)
    } finally {
      setBusy(false)
      setProgress(null)
    }
  }

  async function cancel() {
    try {
      const { cancelTextSignatures } = await import('../api')
      await cancelTextSignatures()
    } catch {
      // 取消失败不打扰用户：批处理自己会结束
    }
  }

  const available = status?.available === true
  const computed = coverage?.computed ?? 0
  const total = coverage?.total ?? 0
  const percent = total > 0 ? Math.round((computed / total) * 100) : 0

  return (
    <div className="msg">
      <h3 style={{ marginTop: 0 }}>文本塔（prompt 语义）</h3>
      <p className="hint">
        在本机给每条 prompt 算一个语义向量，用于<b>语义档</b>聚类与后续的双塔融合。
        它<b>不读图</b>，只吃 prompt 文本 —— 所以比识图快得多（1000 条约十几秒）。
        全程离线，不上传任何文本。
      </p>
      {!available && status && (
        <p className="hint">
          未找到文本塔资源（需要 text_bge.onnx 与 tokenizer_bge.json，缺任一份都不可用）。
          请运行 <code>scripts\download_models.ps1</code> 后重启应用。
        </p>
      )}
      {!available && !status && (
        <p className="hint">文本塔在桌面端运行（本页在浏览器里只做预览）。</p>
      )}
      {available && (
        <p className="hint">
          模型 <code>{status?.model_id}</code> · 分词器词表 {status?.vocab_size ?? '—'} 项
        </p>
      )}
      <div className="row">
        <button
          className="btn-secondary"
          type="button"
          disabled={busy || !available || currentSourceId == null}
          onClick={() => void run()}
        >
          {busy ? '计算中…' : '补齐缺失'}
        </button>
        {busy && (
          <button className="btn-tertiary" type="button" onClick={() => void cancel()}>
            取消
          </button>
        )}
        {total > 0 && (
          <span className="muted">
            已算 {computed} / {total} 条（{percent}%）
          </span>
        )}
        {progress && progress.total > 0 && (
          <span className="muted">
            {progress.done} / {progress.total}
          </span>
        )}
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
                <button className="btn-danger" type="button" disabled={busy} onClick={() => void remove(r.id)}>删除</button>
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
        <button className="btn-secondary" type="button" disabled={busy || loading} onClick={() => void add()}>
          添加
        </button>
      </div>

      <div className="row">
        <button className="btn-tertiary" type="button" onClick={() => setView('groups')}>
          返回
        </button>
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
        <BuildStamp />
      </div>

      <ClusterSection />
      <SignatureSection />
      <TextSignatureSection />
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
        className="btn-secondary"
        type="button"
        disabled={busy}
        onClick={() => void run('csv')}
      >
        导出 CSV
      </button>
      <button
        className="btn-secondary"
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
        className="btn-secondary"
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
