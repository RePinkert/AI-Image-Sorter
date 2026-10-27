import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import Database from 'better-sqlite3'
import fs from 'node:fs'
import path from 'node:path'
import xxhash from 'xxhash-wasm'

import { maskText } from './tools/l2lab/mask.mjs'

const xx = await xxhash()

const isTauri = (req: { headers: { origin?: string } }) =>
  req.headers.origin?.startsWith('tauri://') || req.headers.origin?.startsWith('http://tauri.localhost')

// ---------------------------------------------------------------------------
// Placeholder image mode (copyright safety for the screenshot instrument).
//
// The user owns the generated images; screenshots of the real library would
// embed copyrighted content. The dev bridge therefore serves a DETERMINISTIC
// SVG stand-in instead of the original bytes whenever the request asks for
// it (`?ph=1`, which the front end forwards from the page URL `?ph=1`).
//
// Determinism matters for a second reason: identical DB state → identical
// bytes → screenshots become a real pixel-diff regression baseline.
//
// The placeholder keeps the REAL width/height from the DB, so the masonry
// layout, card sizes and every layout/overflow measurement stay identical to
// production. Only the pixels inside the frame differ.
//
// PROMPT SAFETY (stage six guardrail P1): the same `?ph=1` channel also masks
// the prompt text. The images are the user's copyrighted output, but the
// PROMPTS are private content too — the real prompt never belongs in a
// screenshot, a probe report or a log line. Masking is width-preserving
// (〇 is full-width like a han character, x is half-width like a latin letter),
// so every layout measurement stays comparable to production.
// ---------------------------------------------------------------------------
function fnv1a(input: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i)
    h = Math.imul(h, 0x01000193) >>> 0
  }
  return h >>> 0
}

function xmlEscape(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&apos;')
}

function placeholderSvg(absPath: string, filename: string, width: number, height: number): string {
  const w = Math.max(1, Math.round(width))
  const h = Math.max(1, Math.round(height))
  const hash = fnv1a(absPath)
  const hue = hash % 360
  const hue2 = (hue + 42) % 360
  const label = xmlEscape(filename.length > 42 ? `${filename.slice(0, 40)}…` : filename)
  const fontSize = Math.max(11, Math.min(20, Math.round(Math.min(w, h) / 16)))
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">`,
    '<defs>',
    `<linearGradient id="g" x1="0" y1="0" x2="1" y2="1">`,
    `<stop offset="0" stop-color="hsl(${hue} 16% 27%)"/>`,
    `<stop offset="1" stop-color="hsl(${hue2} 13% 15%)"/>`,
    '</linearGradient>',
    '</defs>',
    `<rect width="${w}" height="${h}" fill="url(#g)"/>`,
    `<circle cx="${Math.round(w * 0.78)}" cy="${Math.round(h * 0.24)}" r="${Math.round(Math.min(w, h) * 0.13)}" fill="hsl(${hue2} 22% 40%)" opacity="0.5"/>`,
    `<text x="${Math.round(w / 2)}" y="${Math.round(h / 2)}" fill="#e6e6ea" opacity="0.62" font-family="monospace" font-size="${fontSize}" text-anchor="middle" dominant-baseline="middle">${label}</text>`,
    `<text x="${Math.round(w / 2)}" y="${Math.round(h / 2 + fontSize * 1.6)}" fill="#9a9aa8" opacity="0.5" font-family="monospace" font-size="${Math.max(10, fontSize - 3)}" text-anchor="middle">${w}×${h}</text>`,
    '</svg>',
  ].join('')
}

function webBridge() {
  return {
    name: 'local-image-sorter-web-bridge',
    configureServer(server: { middlewares: { use: (handler: (req: any, res: any, next: any) => void) => void } }) {
      server.middlewares.use((req, res, next) => {
        if (!req.url?.startsWith('/api/')) return next()
        if (isTauri(req)) return next()

        // The dev bridge reads the **real** library, read-only. Which file is
        // "the real one" depends on how the app was launched: the portable
        // build (marker next to the exe) keeps its library in `exe/data/`,
        // the installed build uses the OS app-data dir. Prefer the portable
        // one when it exists — otherwise the dev preview silently shows a
        // **stale copy** (that is exactly how the appdata copy became a
        // months-old orphan during round 29).
        const portableDb = path.join(
          process.cwd(),
          'src-tauri', 'target', 'release', 'data', 'ai-image-sorter.db',
        )
        const installedDb = path.join(
          process.env.APPDATA ?? path.join(process.env.USERPROFILE ?? '', 'AppData', 'Roaming'),
          'com.aiimagesorter.app',
          'ai-image-sorter.db',
        )
        const dbPath = fs.existsSync(portableDb) ? portableDb : installedDb
        const send = (status: number, body: unknown, contentType = 'application/json') => {
          res.statusCode = status
          res.setHeader('Content-Type', contentType)
          res.end(contentType === 'application/json' ? JSON.stringify(body) : body)
        }

        try {
          const url = new URL(req.url, 'http://localhost')
          // Prompt masking rides the same channel as the placeholder image:
          // `?ph=1` means "this output may be captured by an instrument".
          const maskPrompts = url.searchParams.get('ph') === '1'
          const maskIf = (value: unknown) => (maskPrompts ? maskText(String(value ?? '')) : value)
          const db = new Database(dbPath, { readonly: true, fileMustExist: true })
          const tableExists = (name: string) => {
            const row = db.prepare("SELECT COUNT(*) AS count FROM sqlite_master WHERE type='table' AND name=?").get(name) as { count: number }
            return row.count > 0
          }
          const imageColumns = new Set(
            (db.prepare("SELECT name FROM pragma_table_info('images')").all() as Array<{ name: string }>).map((row) => row.name),
          )
          const sourceCols = new Set(
            (db.prepare("SELECT name FROM pragma_table_info('sources')").all() as Array<{ name: string }>).map((row) => row.name),
          )
          // v16: Model偏差 is a SINGLE wheel-selected dimension with exact
          // matching. Mirrors db.rs dimension_label_of formatting.
          const DIMENSIONS = ['base_model', 'text_encoder', 'vae', 'lora', 'embedding', 'controlnet']
          const UNKNOWN = '(unknown)'
          function l1ModelParam(dimension: string, row: { diffusion_model?: unknown; checkpoint?: unknown; text_encoder?: unknown; vae?: unknown; loras?: unknown; controlnet?: unknown }): string {
            const raw = (v: unknown) => (typeof v === 'string' && v.length > 0 ? v : UNKNOWN)
            let loraNames: string[] = []
            try {
              const parsed = JSON.parse(typeof row.loras === 'string' ? row.loras : '[]')
              loraNames = Array.isArray(parsed)
                ? [...new Set((parsed as Array<{ name?: string }>).map((l) => (l?.name ?? '').trim()).filter(Boolean))]
                : []
            } catch { loraNames = [] }
            loraNames.sort()
            switch (dimension) {
              case 'text_encoder': return raw(row.text_encoder)
              case 'vae': return raw(row.vae)
              case 'lora':
                return loraNames.length === 1
                  ? loraNames[0]
                  : loraNames.length > 1 ? `${loraNames.join(' + ')} (×${loraNames.length})` : UNKNOWN
              case 'controlnet': return raw(row.controlnet)
              default: return raw(typeof row.diffusion_model === 'string' && (row.diffusion_model as string).length > 0
                ? row.diffusion_model as string
                : row.checkpoint)
            }
          }
          const hasWorkflowJoin = tableExists('workflow_templates') && imageColumns.has('workflow_template_id')
          const workflowJoin = hasWorkflowJoin
            ? 'LEFT JOIN workflow_templates wt ON wt.id = i.workflow_template_id'
            : ''
          const workflowName = hasWorkflowJoin ? 'MIN(wt.name)' : 'NULL'
          const imageWorkflowName = hasWorkflowJoin ? 'wt.name' : 'NULL'
          const diffusionModel = imageColumns.has('diffusion_model')
            ? "COALESCE(NULLIF(i.diffusion_model, ''), '')"
            : "COALESCE(NULLIF(i.checkpoint, ''), '')"
          const level = Number(url.searchParams.get('level') ?? 3)
          const keyColumn = ['group_key_l0', 'group_key_l1', 'group_key_l2', 'group_key_l3'][level] ?? 'group_key_l3'

          if (url.pathname === '/api/sources') {
            const srcSel = [
              'id', 'path', 'kind', 'alias', 'scanned_at',
              ...(sourceCols.has('l2_threshold') ? ['l2_threshold'] : []),
              ...(sourceCols.has('model_deviation_dimensions') ? ['model_deviation_dimensions'] : []),
            ]
            const srcRows = db.prepare(`SELECT ${srcSel.join(', ')} FROM sources ORDER BY id`).all() as Array<Record<string, unknown>>
            for (const row of srcRows) {
              if (typeof row.model_deviation_dimensions === 'string') {
                try { row.model_deviation_dimensions = JSON.parse(row.model_deviation_dimensions) } catch { row.model_deviation_dimensions = [] }
              }
              if (!Array.isArray(row.model_deviation_dimensions)) row.model_deviation_dimensions = ['base_model']
            }
            send(200, srcRows)
          } else if (url.pathname === '/api/labels') {
            send(200, db.prepare('SELECT id, name, gesture, color FROM labels ORDER BY id').all())
          } else if (url.pathname === '/api/l2-signature-coverage') {
            // 第 30 轮：签名覆盖率（同步循环不再偷偷补算 → 缺向量的图保持
            // 原组 → 界面要如实说出来）。口径与后端 `l2_signature_coverage`
            // 一致：文本档不需要向量 → slot 为 null。
            const sourceId = Number(url.searchParams.get('sourceId') ?? 0)
            const mode = sourceCols.has('l2_mode')
              ? String((db.prepare('SELECT l2_mode FROM sources WHERE id=?').get(sourceId) as { l2_mode?: string } | undefined)?.l2_mode ?? 'text')
              : 'text'
            const spec = mode === 'visual'
              ? { slot: 'vision', modelId: 'clip-vit-b32-q' }
              : mode === 'semantic'
                ? { slot: 'text', modelId: 'bge-small-zh-q' }
                : null
            if (!spec || !tableExists('image_signatures')) {
              send(200, { mode, slot: spec?.slot ?? null, model_id: spec?.modelId ?? null, total: 0, computed: 0 })
            } else {
              const total = spec.slot === 'text'
                ? (db.prepare("SELECT COUNT(*) AS count FROM images WHERE source_id=? AND prompt_pos != ''").get(sourceId) as { count: number }).count
                : (db.prepare('SELECT COUNT(*) AS count FROM images WHERE source_id=?').get(sourceId) as { count: number }).count
              const computed = (db.prepare(
                'SELECT COUNT(*) AS count FROM image_signatures s JOIN images i ON i.id=s.image_id WHERE i.source_id=? AND s.slot=? AND s.model_id=?',
              ).get(sourceId, spec.slot, spec.modelId) as { count: number }).count
              send(200, { mode, slot: spec.slot, model_id: spec.modelId, total, computed })
            }
          } else if (url.pathname === '/api/workflow-templates') {
            if (!tableExists('workflow_templates')) {
              send(200, [])
              db.close()
              return
            }
            const fromTable = db.prepare('SELECT id, name, path, workflow_id, topology_signature, graph_json, node_count, diffusion_models, model_chain FROM workflow_templates ORDER BY name').all() as Array<Record<string, unknown>>
            // The desktop app stores the numeric array columns as JSON text;
            // the frontend expects real arrays.
            for (const row of fromTable) {
              for (const col of ['diffusion_models', 'model_chain']) {
                if (typeof row[col] === 'string') {
                  try { row[col] = JSON.parse(row[col] as string) } catch { row[col] = [] }
                }
                if (!Array.isArray(row[col])) row[col] = []
              }
              if (row.node_count == null) row.node_count = 0
            }
            if (fromTable.length > 0) {
              send(200, fromTable)
              db.close()
              return
            }
            const sources = db.prepare('SELECT path FROM sources').all() as Array<{ path: string }>
            const templates: unknown[] = []
            const visit = (directory: string) => {
              if (!fs.existsSync(directory)) return
              for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
                const fullPath = path.join(directory, entry.name)
                if (entry.isDirectory()) visit(fullPath)
                else if (entry.isFile() && path.extname(entry.name).toLowerCase() === '.json') {
                  try {
                    const root = JSON.parse(fs.readFileSync(fullPath, 'utf8'))
                    if (!Array.isArray(root.nodes)) continue
                    const nodeTypes = root.nodes.map((node: any) => String(node.type ?? '')).filter(Boolean).sort()
                    const typeById = new Map(root.nodes.map((node: any) => [Number(node.id), String(node.type ?? '')]))
                    const edges = (Array.isArray(root.links) ? root.links : []).map((link: any[]) => {
                      const source = typeById.get(Number(link[1]))
                      const target = typeById.get(Number(link[3]))
                      if (!source || !target) return null
                      return `${source}->${target}`
                    }).filter((e: unknown): e is string => typeof e === 'string').sort()
                    const graphJson = JSON.stringify({ t: nodeTypes, e: edges })
                    const diffusionModels = root.nodes
                      .filter((node: any) => ['UNETLoader', 'UnetLoaderGGUF', 'CheckpointLoaderSimple', 'CheckpointLoader'].includes(node.type))
                      .map((node: any) => node.widgets_values?.[0])
                      .filter((value: unknown): value is string => typeof value === 'string' && value.length > 0)
                    templates.push({ name: path.basename(fullPath, '.json'), path: fullPath,
                      workflow_id: typeof root.id === 'string' ? root.id : null,
                      topology_signature: xx.h64ToString(graphJson),
                      graph_json: graphJson,
                      node_count: root.nodes.length, diffusion_models: diffusionModels, model_chain: [] })
                  } catch { /* Ignore unrelated JSON files. */ }
                }
              }
            }
            const workflowRoots = new Set<string>([
              ...sources.map((source) => path.join(path.dirname(source.path), 'user')),
              'C:/cc/ComfyUI_windows_portable/ComfyUI/user',
            ])
            for (const root of workflowRoots) visit(root)
            send(200, templates)
          } else if (url.pathname === '/api/groups') {
            const sourceId = url.searchParams.get('sourceId')
            const where = sourceId ? `i.source_id = ${Number(sourceId)} AND` : ''
            // v16 aggregates back the L1 model-parameter label.
            const extraSel = level === 1
              ? [
                  imageColumns.has('diffusion_model') ? "MIN(COALESCE(i.diffusion_model,'')) AS diffusion_model" : "'' AS diffusion_model",
                  imageColumns.has('text_encoder') ? "MIN(COALESCE(i.text_encoder,'')) AS text_encoder" : "'' AS text_encoder",
                  imageColumns.has('controlnet') ? "MIN(COALESCE(i.controlnet,'')) AS controlnet" : "'' AS controlnet",
                  "MIN(COALESCE(i.vae,'')) AS vae",
                  "MIN(COALESCE(i.loras,'[]')) AS loras",
                ]
              : []
            const rows = db.prepare(`
              SELECT ${keyColumn} AS group_key, COUNT(*) AS count,
                     MIN(i.prompt_pos) AS prompt_pos, MIN(i.checkpoint) AS checkpoint,
                     MIN(i.source_kind) AS source_kind, MIN(s.path) AS source_path,
                     ${workflowName} AS workflow_name
                     ${extraSel.length > 0 ? ', ' + extraSel.join(', ') : ''}
              FROM images i JOIN sources s ON s.id = i.source_id
              ${workflowJoin}
              WHERE ${where} ${keyColumn} IS NOT NULL AND i.hidden = 0
              GROUP BY ${keyColumn} ORDER BY count DESC
            `).all() as Array<Record<string, unknown> & { group_key: string }>
            if (level === 1) {
              const facetColumn = imageColumns.has('diffusion_model') ? 'diffusion_model' : 'checkpoint'
              const facetStmt = db.prepare(`SELECT COALESCE(NULLIF(${facetColumn}, ''), '(未知)') AS model, COUNT(*) AS count
                FROM images WHERE group_key_l1 = ? AND hidden = 0 GROUP BY model ORDER BY count DESC`)
              // Resolve the SINGLE selected dimension for label rendering.
              let dimension = 'base_model'
              if (sourceId) {
                const src = db.prepare('SELECT model_deviation_dimensions FROM sources WHERE id = ?').get(Number(sourceId)) as { model_deviation_dimensions?: string } | undefined
                try {
                  const list = JSON.parse(src?.model_deviation_dimensions ?? '[]') as string[]
                  const hit = (Array.isArray(list) ? list : []).find((d) => (DIMENSIONS as string[]).includes(d))
                  if (hit) dimension = hit
                } catch { /* keep default */ }
              }
              for (const group of rows) {
                group.model_facets = facetStmt.all(group.group_key)
                if (sourceId) {
                  group.model_param = l1ModelParam(dimension, group as Record<string, unknown>)
                }
              }
            }
            // The group card renders `prompt_pos` as the group's name — the
            // most privacy-sensitive string in the whole UI. Mask it in the
            // capture channel; the desktop app is unaffected.
            for (const row of rows) row.prompt_pos = maskIf(row.prompt_pos)
            send(200, rows)
          } else if (url.pathname === '/api/group-thumbnails') {
            const keys = (url.searchParams.get('keys') ?? '').split(',').filter(Boolean)
            const statement = db.prepare(`SELECT ${keyColumn} AS group_key, abs_path
              FROM images WHERE ${keyColumn} = ? AND hidden = 0 ORDER BY seed, filename LIMIT 4`)
            const result = keys.map((key) => ({
              group_key: key,
              thumb_paths: statement.all(key).map((row: any) => row.abs_path),
            })).filter((item) => item.thumb_paths.length > 0)
            send(200, result)
          } else if (url.pathname === '/api/move-targets') {
            // 移动对话框的候选组（与 db.rs 的 list_l2_targets 同口径）。
            // 这条端点之前一直缺着 —— 结果是「移动至其他组」在浏览器里
            // 永远 404，界面根本点不开（探针也就永远验不到它）。
            const sourceId = Number(url.searchParams.get('sourceId') ?? 0)
            const currentL2 = url.searchParams.get('currentL2') ?? ''
            const rows = db.prepare(`SELECT i.group_key_l2 AS group_key, COUNT(*) AS count,
                MIN(i.prompt_pos) AS prompt_pos, MIN(i.checkpoint) AS checkpoint,
                MIN(i.source_kind) AS source_kind, MIN(s.path) AS source_path,
                ${workflowName} AS workflow_name
              FROM images i JOIN sources s ON s.id=i.source_id
              ${workflowJoin}
              WHERE i.source_id=? AND i.group_key_l2 IS NOT NULL
                AND i.group_key_l2 != ? AND i.hidden=0
              GROUP BY i.group_key_l2 ORDER BY count DESC`)
              .all(sourceId, currentL2) as Array<Record<string, unknown>>
            send(200, rows.map((row) => ({ ...row, prompt_pos: maskIf(row.prompt_pos ?? '') })))
          } else if (url.pathname === '/api/suggest-move-targets') {
            // 「可能相关」推荐（阶段六）。桌面端用 Rust 侧的同一套口径；
            // dev bridge 这里实现同样的两种基准：有视觉签名用视觉余弦，
            // 没有就退回 prompt 的词 Jaccard（与 clustering.rs 的 tokenize 同口径）。
            const sourceId = Number(url.searchParams.get('sourceId') ?? 0)
            const currentL2 = url.searchParams.get('currentL2') ?? ''
            const imageIds = (url.searchParams.get('imageIds') ?? '')
              .split(',')
              .map((x) => Number(x))
              .filter((x) => Number.isFinite(x) && x > 0)
            // 与 clustering.rs:15 同口径的词切分：逗号/空白切分，CJK 逐字。
            const tokenize = (prompt: string): string[] => {
              const tokens: string[] = []
              for (const part of prompt.toLowerCase().split(/[,\s]+/)) {
                if (!part) continue
                if (/[\u4e00-\u9fff]/.test(part)) {
                  let buf = ''
                  for (const ch of part) {
                    if (/[\u4e00-\u9fff]/.test(ch)) {
                      if (buf) { tokens.push(buf); buf = '' }
                      tokens.push(ch)
                    } else if (/[0-9a-z']/i.test(ch) || ch === '-') {
                      buf += ch
                    } else if (buf) { tokens.push(buf); buf = '' }
                  }
                  if (buf) tokens.push(buf)
                } else {
                  tokens.push(part)
                }
              }
              return tokens
            }
            if (imageIds.length === 0) {
              send(200, [])
            } else {
              const groupRows = db.prepare(`SELECT group_key_l2 AS k, COUNT(*) AS n, MIN(prompt_pos) AS p
                FROM images WHERE source_id = ? AND group_key_l2 IS NOT NULL AND hidden = 0
                GROUP BY group_key_l2`).all(sourceId) as Array<{ k: string; n: number; p: string | null }>
              const memberStmt = db.prepare(`SELECT id FROM images
                WHERE source_id = ? AND group_key_l2 = ? AND hidden = 0`)
              const hasSignatures = tableExists('image_signatures')
              const vectors = new Map<number, Float32Array>()
              if (hasSignatures) {
                const vecStmt = db.prepare(`SELECT image_id, vec FROM image_signatures
                  WHERE slot = 'vision' AND model_id = 'clip-vit-b32-q'`)
                for (const row of vecStmt.all() as Array<{ image_id: number; vec: Buffer }>) {
                  const bytes = row.vec
                  if (bytes.length % 4 !== 0) continue
                  const arr = new Float32Array(bytes.length / 4)
                  for (let i = 0; i < arr.length; i++) {
                    arr[i] = bytes.readFloatLE(i * 4)
                  }
                  vectors.set(row.image_id, arr)
                }
              }
              // 文本基准：每个图的词集合
              const tokenStmt = db.prepare(`SELECT id, prompt_pos FROM images
                WHERE source_id = ? AND hidden = 0 AND prompt_pos != ''`)
              const tokenSets = new Map<number, Set<string>>()
              for (const row of tokenStmt.all(sourceId) as Array<{ id: number; prompt_pos: string }>) {
                tokenSets.set(row.id, new Set(tokenize(row.prompt_pos)))
              }
              const useVision = imageIds.some((id) => vectors.has(id))
              const dot = (a: Float32Array, b: Float32Array) => {
                let sum = 0
                const n = Math.min(a.length, b.length)
                for (let i = 0; i < n; i++) sum += a[i] * b[i]
                return sum
              }
              const jaccard = (a: Set<string>, b: Set<string>) => {
                if (!a.size && !b.size) return 1
                let inter = 0
                for (const t of a) if (b.has(t)) inter++
                const union = a.size + b.size - inter
                return union === 0 ? 1 : inter / union
              }
              const out = groupRows
                .filter((g) => g.k !== currentL2)
                .map((g) => {
                  const memberIds = (memberStmt.all(sourceId, g.k) as Array<{ id: number }>).map((m) => m.id)
                  let total = 0
                  let counted = 0
                  for (const id of imageIds) {
                    let best = -Infinity
                    if (useVision) {
                      const vec = vectors.get(id)
                      if (!vec) continue
                      for (const mid of memberIds) {
                        const mv = vectors.get(mid)
                        if (mv) best = Math.max(best, dot(vec, mv))
                      }
                    } else {
                      const tok = tokenSets.get(id)
                      if (!tok) continue
                      for (const mid of memberIds) {
                        const mt = tokenSets.get(mid)
                        if (mt) best = Math.max(best, jaccard(tok, mt))
                      }
                    }
                    if (Number.isFinite(best)) { total += best; counted += 1 }
                  }
                  if (!counted) return null
                  return {
                    group_key: g.k,
                    count: g.n,
                    prompt_pos: maskIf(g.p ?? ''),
                    score: total / counted,
                    basis: useVision ? 'vision' : 'text',
                  }
                })
                .filter((x): x is NonNullable<typeof x> => x !== null)
                .sort((a, b) => b.score - a.score)
                .slice(0, 12)
              send(200, out)
            }
          } else if (url.pathname === '/api/group-outliers') {
            // 组内「疑似错分」的图（阶段六 Round 4）。桌面端用 Rust 侧同一套口径：
            // 组内每个成员与其它成员的最大余弦，低于门槛的就是「没有够格伙伴」。
            const groupKey = url.searchParams.get('groupKey') ?? ''
            const threshold = Number(url.searchParams.get('threshold') ?? 0.75)
            if (!tableExists('image_signatures')) {
              send(200, [])
            } else {
              const vecStmt = db.prepare(`SELECT image_id, vec FROM image_signatures
                WHERE slot = 'vision' AND model_id = 'clip-vit-b32-q'`)
              const vectors = new Map<number, Float32Array>()
              for (const row of vecStmt.all() as Array<{ image_id: number; vec: Buffer }>) {
                const bytes = row.vec
                if (bytes.length % 4 !== 0) continue
                const arr = new Float32Array(bytes.length / 4)
                for (let i = 0; i < arr.length; i++) arr[i] = bytes.readFloatLE(i * 4)
                vectors.set(row.image_id, arr)
              }
              const memberStmt = db.prepare(`SELECT id FROM images
                WHERE group_key_l2 = ? AND hidden = 0 ORDER BY id`)
              const members = (memberStmt.all(groupKey) as Array<{ id: number }>)
                .map((m) => m.id)
                .filter((id) => vectors.has(id))
              const dot = (a: Float32Array, b: Float32Array) => {
                let sum = 0
                const n = Math.min(a.length, b.length)
                for (let i = 0; i < n; i++) sum += a[i] * b[i]
                return sum
              }
              const out: Array<Record<string, unknown>> = []
              for (const id of members) {
                let best = -Infinity
                for (const other of members) {
                  if (other === id) continue
                  best = Math.max(best, dot(vectors.get(id)!, vectors.get(other)!))
                }
                if (best < threshold) {
                  out.push({
                    image_id: id,
                    max_similarity: best,
                    best_group_key: null,
                    best_score: 0,
                  })
                }
              }
              out.sort((a, b) => (a.max_similarity as number) - (b.max_similarity as number))
              send(200, out)
            }
          } else if (url.pathname === '/api/group-images') {
            const groupKey = url.searchParams.get('groupKey') ?? ''
            const includeHidden = url.searchParams.get('includeHidden') === '1'
            const hidden = includeHidden ? '' : 'AND hidden = 0'
            const rows = db.prepare(`SELECT i.id, i.source_id, i.abs_path, i.filename, i.width, i.height,
              i.prompt_pos, i.prompt_neg, i.checkpoint, i.loras, i.vae, i.samplers, i.seed,
              i.meta_ok, i.source_kind, i.hidden, i.size,
              ${diffusionModel} AS diffusion_model, ${imageWorkflowName} AS workflow_name,
              (SELECT internal_score FROM scores WHERE scores.image_id = i.id) AS score
              FROM images i ${workflowJoin}
              WHERE i.${keyColumn} = ? ${hidden} ORDER BY i.seed, i.filename`).all(groupKey)
            send(200, rows.map((row: any) => ({
              ...row,
              prompt_pos: maskIf(row.prompt_pos),
              prompt_neg: maskIf(row.prompt_neg),
              labels: [],
              hidden: Boolean(row.hidden),
              meta_ok: Boolean(row.meta_ok),
            })))
          } else if (url.pathname === '/api/recommend-prompts') {
            if (level >= 3) {
              send(200, [])
            } else if (!imageColumns.has('generation_recipe_json')) {
              // Legacy databases must be migrated by the desktop app. The
              // read-only dev bridge never mutates production schema.
              send(200, [])
            } else {
              const groupKey = url.searchParams.get('groupKey') ?? ''
              const offset = Math.max(0, Number(url.searchParams.get('offset') ?? 0))
              const limit = Math.min(100, Math.max(1, Number(url.searchParams.get('limit') ?? 10)))
              const rows = db.prepare(`SELECT i.id, i.prompt_pos AS prompt_text,
                COALESCE(i.prompt_neg, '') AS prompt_neg,
                i.generation_recipe_json,
                COALESCE(s.internal_score, 50.0) AS score
                FROM images i LEFT JOIN scores s ON s.image_id = i.id
                WHERE i.${keyColumn} = ? AND i.hidden = 0 AND i.prompt_pos != ''
                  AND i.generation_recipe_json IS NOT NULL
                  AND i.generation_recipe_json != '' AND i.generation_recipe_json != '{}'`).all(groupKey) as Array<{
                    id: number
                    prompt_text: string
                    prompt_neg: string
                    generation_recipe_json: string
                    score: number
                  }>
              const grouped = new Map<string, {
                prompt_text: string
                prompt_neg: string
                recipe: Record<string, any>
                samples: Array<{ id: number; score: number }>
              }>()
              for (const row of rows) {
                try {
                  const recipe = JSON.parse(row.generation_recipe_json) as Record<string, any>
                  const complete = Boolean(recipe.checkpoint || recipe.diffusion_model)
                    && Boolean(recipe.sampler) && Boolean(recipe.scheduler)
                    && Number(recipe.steps) > 0 && Number.isFinite(Number(recipe.cfg))
                    && Number(recipe.width) > 0 && Number(recipe.height) > 0
                  if (!complete) continue
                  const key = JSON.stringify([row.prompt_text, row.prompt_neg, recipe])
                  const entry = grouped.get(key) ?? {
                    prompt_text: row.prompt_text,
                    prompt_neg: row.prompt_neg,
                    recipe,
                    samples: [],
                  }
                  entry.samples.push({ id: row.id, score: Number.isFinite(row.score) ? row.score : 50 })
                  grouped.set(key, entry)
                } catch {
                  // Ignore malformed legacy recipe rows.
                }
              }
              const ranked = Array.from(grouped.values()).map((entry) => {
                const scores = entry.samples.map((sample) => sample.score).sort((a, b) => a - b)
                const sampleCount = scores.length
                const avgScore = scores.reduce((sum, score) => sum + score, 0) / sampleCount
                const middle = Math.floor(sampleCount / 2)
                const medianScore = sampleCount % 2 === 1 ? scores[middle] : (scores[middle - 1] + scores[middle]) / 2
                const scoreVariance = scores.reduce((sum, score) => sum + (score - avgScore) ** 2, 0) / sampleCount
                const confidence = sampleCount / (sampleCount + 5)
                const shrinkScore = avgScore * confidence + 50 * (1 - confidence)
                const examples = [...entry.samples].sort((a, b) => b.score - a.score || a.id - b.id).slice(0, 4)
                return {
                  rank: shrinkScore,
                  prompt_text: entry.prompt_text,
                  prompt_neg: entry.prompt_neg,
                  diffusion_model: String(entry.recipe.diffusion_model ?? ''),
                  checkpoint: String(entry.recipe.checkpoint ?? ''),
                  loras: Array.isArray(entry.recipe.loras) ? entry.recipe.loras : [],
                  vae: String(entry.recipe.vae ?? ''),
                  sampler: String(entry.recipe.sampler ?? ''),
                  scheduler: String(entry.recipe.scheduler ?? ''),
                  steps: Number(entry.recipe.steps),
                  cfg: Number(entry.recipe.cfg),
                  width: Number(entry.recipe.width),
                  height: Number(entry.recipe.height),
                  aspect_ratio: Number(entry.recipe.aspect_ratio) || Number(entry.recipe.width) / Number(entry.recipe.height),
                  sample_count: sampleCount,
                  max_score: Math.max(...scores),
                  avg_score: avgScore,
                  median_score: medianScore,
                  score_variance: scoreVariance,
                  confidence,
                  example_image_ids: examples.map((sample) => sample.id),
                  image_count: sampleCount,
                }
              }).sort((a, b) => b.rank - a.rank || b.avg_score - a.avg_score || b.sample_count - a.sample_count)
              // Mask AFTER grouping: the grouping key is the raw prompt, and
              // masking first would collapse distinct prompts into one entry.
              send(200, ranked.slice(offset, offset + limit).map(({ rank: _rank, ...item }) => ({
                ...item,
                prompt_text: maskIf(item.prompt_text),
                prompt_neg: maskIf(item.prompt_neg),
              })))
            }
          } else if (url.pathname === '/api/arena-suggested') {
            const left = Number(url.searchParams.get('left'))
            const right = Number(url.searchParams.get('right'))
            const score = (id: number) => (db.prepare('SELECT internal_score FROM scores WHERE image_id = ?').get(id) as { internal_score?: number } | undefined)?.internal_score ?? 50
            send(200, Math.abs(score(left) - score(right)) < 5)
          } else if (url.pathname === '/api/file') {
            const filePath = url.searchParams.get('path')
            const placeholder = url.searchParams.get('ph') === '1'
            if (!filePath) send(400, { error: 'missing path' })
            else {
              const ext = path.extname(filePath).toLowerCase()
              const types: Record<string, string> = { '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.bmp': 'image/bmp' }
              const registered = db.prepare('SELECT COUNT(*) AS count FROM images WHERE abs_path = ?').get(filePath) as { count: number }
              if (!registered.count) send(403, { error: 'file is not registered' })
              else if (!types[ext]) send(415, { error: 'unsupported image type' })
              else if (placeholder) {
                // Copyright-safe stand-in: never read the original bytes.
                const row = db.prepare('SELECT filename, width, height FROM images WHERE abs_path = ?').get(filePath) as
                  { filename?: string; width?: number | null; height?: number | null } | undefined
                const svg = placeholderSvg(
                  filePath,
                  row?.filename ?? path.basename(filePath),
                  Number(row?.width ?? 0) || 512,
                  Number(row?.height ?? 0) || 512,
                )
                send(200, svg, 'image/svg+xml')
              } else if (!fs.existsSync(filePath)) send(404, { error: 'file not found' })
              else send(200, fs.readFileSync(filePath), types[ext])
            }
          } else {
            send(404, { error: 'unknown api route' })
          }
          db.close()
        } catch (error) {
          send(500, { error: String(error) })
        }
      })
    },
  }
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), webBridge()],
  server: {
    watch: {
      // Rust 的构建产物与实验产物必须排除在文件监视之外：
      //  · `src-tauri/target/` 会被 cargo 反复改写，Windows 上文件监视器
      //    撞上被占用的 DLL 会抛 EBUSY 并**直接崩掉 dev server**
      //    （实测：dev server 开着时跑一次 cargo test 就崩）
      //  · `tools/l2lab/out/` 是研究产物（向量 JSON），一直在变，
      //    盯着它会让页面无意义地重载
      ignored: [
        '**/src-tauri/target/**',
        '**/tools/l2lab/out/**',
        '**/tools/l2lab/.venv/**',
        '**/src-tauri/resources/models/**',
      ],
    },
  },
})
