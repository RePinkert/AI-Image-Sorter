import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import Database from 'better-sqlite3'
import fs from 'node:fs'
import path from 'node:path'
import xxhash from 'xxhash-wasm'

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

        const dbPath = path.join(
          process.env.APPDATA ?? path.join(process.env.USERPROFILE ?? '', 'AppData', 'Roaming'),
          'com.aiimagesorter.app',
          'ai-image-sorter.db',
        )
        const send = (status: number, body: unknown, contentType = 'application/json') => {
          res.statusCode = status
          res.setHeader('Content-Type', contentType)
          res.end(contentType === 'application/json' ? JSON.stringify(body) : body)
        }

        try {
          const url = new URL(req.url, 'http://localhost')
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
            send(200, rows.map((row: any) => ({ ...row, labels: [], hidden: Boolean(row.hidden), meta_ok: Boolean(row.meta_ok) })))
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
              send(200, ranked.slice(offset, offset + limit).map(({ rank: _rank, ...item }) => item))
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
})
