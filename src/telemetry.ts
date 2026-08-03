import { invoke } from '@tauri-apps/api/core'

export type TelemetryEventName =
  | 'view_enter'
  | 'view_exit'
  | 'swipe_commit'
  | 'arena_vote_commit'
  | 'hide'
  | 'prompt_recommend_open'
  | 'prompt_recommend_copy'
  | 'prompt_recommend_error'
  | 'sync'
  | 'invoke_error'
  | 'unhandled_error'
  | 'dwell_threshold'

type PayloadValue = string | number | boolean | null | undefined
export type TelemetryPayload = Record<string, PayloadValue>

const ENABLED_KEY = 'ai-image-sorter-telemetry-enabled'
const DIAGNOSTICS_KEY = 'ai-image-sorter-diagnostics'
const SCHEMA_VERSION = '1'
const MAX_DIAGNOSTICS = 60
const DWELL_THRESHOLD_MS = 30_000

const sessionId = globalThis.crypto?.randomUUID?.() ??
  `session-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
const idSalt = globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2)

const allowedFields: Record<TelemetryEventName, ReadonlySet<string>> = {
  view_enter: new Set(['view']),
  view_exit: new Set(['view', 'duration_ms']),
  swipe_commit: new Set(['gesture', 'has_label', 'duration_ms', 'image_id']),
  arena_vote_commit: new Set(['side', 'duration_ms', 'left_id', 'right_id']),
  hide: new Set(['hidden', 'mode', 'duration_ms', 'image_id']),
  prompt_recommend_open: new Set(['granularity', 'group_id']),
  prompt_recommend_copy: new Set(['success', 'group_id']),
  prompt_recommend_error: new Set(['error_code', 'retryable', 'group_id']),
  sync: new Set(['success', 'added_count', 'pending_count', 'parse_error_count', 'source_count', 'reclustered', 'duration_ms']),
  invoke_error: new Set(['operation', 'error_code', 'retryable', 'operation_id']),
  unhandled_error: new Set(['source', 'error_code']),
  dwell_threshold: new Set(['view', 'duration_ms']),
}

const enumValues: Record<string, ReadonlySet<string>> = {
  view: new Set(['import', 'groups', 'swipe', 'arena', 'folder', 'settings', 'error']),
  mode: new Set(['swipe', 'arena', 'folder', 'groups', 'import', 'settings', 'unknown']),
  gesture: new Set(['left', 'right', 'up', 'down']),
  side: new Set(['left', 'right']),
  source: new Set(['window_error', 'unhandled_rejection', 'error_boundary']),
  operation: new Set([
    'find_comfy_sources', 'find_workflow_templates', 'refresh_workflow_templates', 'sync_all',
    'add_source_and_scan', 'list_sources', 'list_groups', 'list_group_images',
    'list_group_images_all', 'toggle_hidden_action', 'trash_image', 'recommend_prompts',
    'recluster_source', 'merge_groups', 'split_images', 'list_labels', 'upsert_label',
    'delete_label', 'set_image_label', 'apply_swipe_action', 'undo_review_action', 'arena_vote',
    'arena_suggested', 'export_data', 'archive_copy', 'get_group_thumbnails', 'web_request',
  ]),
  error_code: new Set([
    'UNKNOWN', 'OPERATION_FAILED', 'NETWORK_ERROR', 'HTTP_ERROR', 'NOT_FOUND',
    'DB_BUSY', 'PERMISSION_DENIED', 'UNSUPPORTED', 'INVALID_REQUEST', 'BACKEND_ERROR',
  ]),
}

const numericFields = new Set([
  'duration_ms', 'granularity', 'added_count', 'pending_count', 'parse_error_count', 'source_count',
])
const booleanFields = new Set(['has_label', 'hidden', 'success', 'retryable', 'reclustered'])

interface DiagnosticEntry {
  event: TelemetryEventName
  occurred_at: string
  payload: Record<string, string | number | boolean>
}

function isWebDev() {
  return typeof window !== 'undefined' && !('__TAURI_INTERNALS__' in window)
}

function hashId(value: string | number): string {
  const text = `${idSalt}:${String(value)}`
  let hash = 2166136261
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i)
    hash = Math.imul(hash, 16777619)
  }
  return `h_${(hash >>> 0).toString(16).padStart(8, '0')}`
}

function sanitizePayload(event: TelemetryEventName, payload: TelemetryPayload) {
  const sanitized: Record<string, string | number | boolean> = {}
  for (const [key, value] of Object.entries(payload)) {
    if (value == null || !allowedFields[event].has(key)) continue
    if (key.endsWith('_id')) {
      if (typeof value === 'string' || typeof value === 'number') sanitized[key] = hashId(value)
      continue
    }
    if (numericFields.has(key)) {
      if (typeof value === 'number' && Number.isFinite(value)) sanitized[key] = Math.max(0, Math.round(value))
      continue
    }
    if (booleanFields.has(key)) {
      if (typeof value === 'boolean') sanitized[key] = value
      continue
    }
    const allowed = enumValues[key]
    if (allowed && typeof value === 'string' && allowed.has(value)) sanitized[key] = value
  }
  return sanitized
}

function addDiagnostic(entry: DiagnosticEntry) {
  try {
    const current = JSON.parse(localStorage.getItem(DIAGNOSTICS_KEY) ?? '[]') as DiagnosticEntry[]
    localStorage.setItem(DIAGNOSTICS_KEY, JSON.stringify([...current.slice(-(MAX_DIAGNOSTICS - 1)), entry]))
  } catch {
    // Diagnostics must never affect the primary workflow.
  }
}

function errorCode(error: unknown): string {
  if (error && typeof error === 'object' && 'code' in error) {
    const value = String((error as { code?: unknown }).code ?? '').toUpperCase()
    if (enumValues.error_code.has(value)) return value
  }
  return 'UNKNOWN'
}

export function getTelemetrySessionId() {
  return sessionId
}

export function isTelemetryEnabled() {
  try {
    return localStorage.getItem(ENABLED_KEY) !== 'false'
  } catch {
    return true
  }
}

export function setTelemetryEnabled(enabled: boolean) {
  try {
    localStorage.setItem(ENABLED_KEY, String(enabled))
  } catch {
    // A disabled localStorage backend should not break settings.
  }
}

export function track(event: TelemetryEventName, payload: TelemetryPayload = {}) {
  if (!isTelemetryEnabled()) return
  const occurredAt = new Date().toISOString()
  const sanitized = sanitizePayload(event, payload)
  addDiagnostic({ event, occurred_at: occurredAt, payload: sanitized })
  if (isWebDev()) return
  void invoke('record_telemetry_event', {
    args: {
      session_id: sessionId,
      event_name: event,
      schema_version: SCHEMA_VERSION,
      occurred_at: occurredAt,
      mode: typeof sanitized.mode === 'string' ? sanitized.mode : null,
      payload_json: JSON.stringify(sanitized),
      severity: event.includes('error') ? 'error' : 'info',
    },
  }).catch(() => {})
}

const recentErrors = new Map<string, number>()

export function trackError(
  event: 'invoke_error' | 'unhandled_error' | 'prompt_recommend_error',
  error: unknown,
  payload: TelemetryPayload = {},
) {
  const code = errorCode(error)
  const dedupeKey = `${event}:${String(payload.operation ?? payload.source ?? '')}:${code}`
  const now = Date.now()
  if (now - (recentErrors.get(dedupeKey) ?? 0) < 30_000) return
  recentErrors.set(dedupeKey, now)
  track(event, { ...payload, error_code: code })
}

export function trackView(view: string, phase: 'enter' | 'exit', durationMs?: number) {
  track(phase === 'enter' ? 'view_enter' : 'view_exit', { view, duration_ms: durationMs })
}

export function trackAction(
  event: 'swipe_commit' | 'arena_vote_commit' | 'hide',
  payload: TelemetryPayload,
) {
  track(event, payload)
}

export function trackDwell(view: string, durationMs: number) {
  if (durationMs >= DWELL_THRESHOLD_MS) track('dwell_threshold', { view, duration_ms: durationMs })
}

function downloadDiagnostics(report: unknown) {
  const text = JSON.stringify(report, null, 2)
  const url = URL.createObjectURL(new Blob([text], { type: 'application/json' }))
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = `ai-image-sorter-diagnostics-${new Date().toISOString().slice(0, 10)}.json`
  anchor.click()
  URL.revokeObjectURL(url)
}

export async function exportDiagnostics() {
  let events: DiagnosticEntry[] = []
  try {
    events = JSON.parse(localStorage.getItem(DIAGNOSTICS_KEY) ?? '[]') as DiagnosticEntry[]
  } catch {
    events = []
  }
  let backend: unknown = null
  if (!isWebDev()) {
    try {
      backend = await invoke('export_diagnostics')
    } catch {
      // The local browser report remains useful if the backend is unavailable.
    }
  }
  downloadDiagnostics({
    schema_version: SCHEMA_VERSION,
    session_id: sessionId,
    events,
    backend,
  })
}
