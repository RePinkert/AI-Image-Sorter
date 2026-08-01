import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import Database from 'better-sqlite3'
import fs from 'node:fs'
import path from 'node:path'
import xxhash from 'xxhash-wasm'

const xx = await xxhash()

const isTauri = (req: { headers: { origin?: string } }) =>
  req.headers.origin?.startsWith('tauri://') || req.headers.origin?.startsWith('http://tauri.localhost')

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
          const db = new Database(dbPath)
          for (const column of ['diffusion_model TEXT', "model_chain_json TEXT NOT NULL DEFAULT '[]'", 'workflow_key TEXT', 'workflow_graph_json TEXT', 'workflow_template_id INTEGER', 'workflow_match_confidence REAL']) {
            const name = column.split(' ')[0]
            const exists = db.prepare("SELECT COUNT(*) AS count FROM pragma_table_info('images') WHERE name = ?").get(name) as { count: number }
            if (!exists.count) db.exec(`ALTER TABLE images ADD COLUMN ${column}`)
          }
          db.exec(`CREATE TABLE IF NOT EXISTS workflow_templates (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            workflow_id TEXT,
            topology_signature TEXT NOT NULL,
            graph_json TEXT NOT NULL,
            node_count INTEGER NOT NULL,
            diffusion_models TEXT NOT NULL DEFAULT '[]',
            model_chain TEXT NOT NULL DEFAULT '[]',
            scanned_at TEXT
          )`)
          // Keep the browser bridge aligned with the desktop migration. This
          // is idempotent and fixes databases created before L0 was rebuilt
          // from the normalized source path.
          const sourceRows = db.prepare('SELECT id, path FROM sources').all() as Array<{ id: number; path: string }>
          const updateL0 = db.prepare('UPDATE images SET group_key_l0 = ? WHERE source_id = ?')
          const rebuildL0 = db.transaction(() => {
            for (const source of sourceRows) {
              const normalized = source.path.replaceAll('\\', '/').replace(/\/$/, '')
              updateL0.run(xx.h64ToString(normalized), source.id)
            }
          })
          rebuildL0()
          const level = Number(url.searchParams.get('level') ?? 3)
          const keyColumn = ['group_key_l0', 'group_key_l1', 'group_key_l2', 'group_key_l3'][level] ?? 'group_key_l3'

          if (url.pathname === '/api/sources') {
            send(200, db.prepare('SELECT id, path, kind, alias, scanned_at FROM sources ORDER BY id').all())
          } else if (url.pathname === '/api/labels') {
            send(200, db.prepare('SELECT id, name, gesture, color FROM labels ORDER BY id').all())
          } else if (url.pathname === '/api/workflow-templates') {
            const fromTable = db.prepare('SELECT id, name, path, workflow_id, topology_signature, graph_json, node_count, diffusion_models, model_chain FROM workflow_templates ORDER BY name').all()
            if (fromTable.length > 0) {
              send(200, fromTable)
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
            const rows = db.prepare(`
              SELECT ${keyColumn} AS group_key, COUNT(*) AS count,
                     MIN(i.prompt_pos) AS prompt_pos, MIN(i.checkpoint) AS checkpoint,
                     MIN(i.source_kind) AS source_kind, MIN(s.path) AS source_path,
                     MIN(wt.name) AS workflow_name
              FROM images i JOIN sources s ON s.id = i.source_id
              LEFT JOIN workflow_templates wt ON wt.id = i.workflow_template_id
              WHERE ${where} ${keyColumn} IS NOT NULL AND i.hidden = 0
              GROUP BY ${keyColumn} ORDER BY count DESC
            `).all() as Array<Record<string, unknown> & { group_key: string }>
            if (level === 1) {
              const facetStmt = db.prepare(`SELECT COALESCE(NULLIF(diffusion_model, ''), '(未知)') AS model, COUNT(*) AS count
                FROM images WHERE group_key_l1 = ? AND hidden = 0 GROUP BY model ORDER BY count DESC`)
              for (const group of rows) {
                group.model_facets = facetStmt.all(group.group_key)
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
              COALESCE(NULLIF(i.diffusion_model, ''), '') AS diffusion_model, wt.name AS workflow_name,
              (SELECT internal_score FROM scores WHERE scores.image_id = i.id) AS score
              FROM images i LEFT JOIN workflow_templates wt ON wt.id = i.workflow_template_id
              WHERE i.${keyColumn} = ? ${hidden} ORDER BY i.seed, i.filename`).all(groupKey)
            send(200, rows.map((row: any) => ({ ...row, labels: [], hidden: Boolean(row.hidden), meta_ok: Boolean(row.meta_ok) })))
          } else if (url.pathname === '/api/recommend-prompts') {
            if (level >= 3) {
              send(200, [])
            } else {
              const groupKey = url.searchParams.get('groupKey') ?? ''
              const offset = Math.max(0, Number(url.searchParams.get('offset') ?? 0))
              const limit = Math.min(100, Math.max(1, Number(url.searchParams.get('limit') ?? 10)))
              const rows = db.prepare(`SELECT i.prompt_pos AS prompt_text,
                COALESCE(i.diffusion_model, '') AS diffusion_model,
                MAX(COALESCE(s.internal_score, 50.0)) AS max_score,
                AVG(COALESCE(s.internal_score, 50.0)) AS avg_score,
                COUNT(*) AS image_count
                FROM images i LEFT JOIN scores s ON s.image_id = i.id
                WHERE i.${keyColumn} = ? AND i.hidden = 0 AND i.prompt_pos != ''
                GROUP BY i.prompt_pos, i.diffusion_model ORDER BY max_score DESC
                LIMIT ? OFFSET ?`).all(groupKey, limit, offset)
              send(200, rows)
            }
          } else if (url.pathname === '/api/arena-suggested') {
            const left = Number(url.searchParams.get('left'))
            const right = Number(url.searchParams.get('right'))
            const score = (id: number) => (db.prepare('SELECT internal_score FROM scores WHERE image_id = ?').get(id) as { internal_score?: number } | undefined)?.internal_score ?? 50
            send(200, Math.abs(score(left) - score(right)) < 5)
          } else if (url.pathname === '/api/file') {
            const filePath = url.searchParams.get('path')
            if (!filePath) send(400, { error: 'missing path' })
            else if (!fs.existsSync(filePath)) send(404, { error: 'file not found' })
            else {
              const ext = path.extname(filePath).toLowerCase()
              const types: Record<string, string> = { '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.bmp': 'image/bmp' }
              send(200, fs.readFileSync(filePath), types[ext] ?? 'application/octet-stream')
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
