import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import { confirm, open, save } from '@tauri-apps/plugin-dialog'
import { trackError } from './telemetry'
import type {
  ActionResult,
  FoundSourceDto,
  Granularity,
  GroupInfo,
  GroupThumbDto,
  ImageRow,
  LabelRow,
  ManualGroupResult,
  PromptRecommendation,
  ScanResult,
  SourceRow,
  SyncAllResult,
  UndoActionResult,
  WorkflowTemplateDto,
} from './types'

export interface ActionContext {
  sessionId?: string
  startedAt?: string
  contextSignature?: string
}

export class ApiError extends Error {
  readonly code: string
  readonly userMessage: string
  readonly retryable: boolean
  readonly operationId?: string

  constructor(input: {
    code: string
    userMessage: string
    retryable: boolean
    operationId?: string
    cause?: unknown
  }) {
    super(input.userMessage, { cause: input.cause })
    this.name = 'ApiError'
    this.code = input.code
    this.userMessage = input.userMessage
    this.retryable = input.retryable
    this.operationId = input.operationId
  }
}

export const isWebDev = () => typeof window !== 'undefined' && !('__TAURI_INTERNALS__' in window)

let operationCounter = 0

function nextOperationId() {
  operationCounter += 1
  return `op-${Date.now().toString(36)}-${operationCounter.toString(36)}`
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value != null && typeof value === 'object' ? value as Record<string, unknown> : null
}

function parseStructuredError(error: unknown): Record<string, unknown> | null {
  const direct = asRecord(error)
  if (direct) return direct
  if (typeof error !== 'string') return null
  try {
    return asRecord(JSON.parse(error))
  } catch {
    return null
  }
}

function rawErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  const record = asRecord(error)
  if (record) {
    const value = record.user_message ?? record.userMessage ?? record.message ?? record.error
    if (typeof value === 'string') return value
  }
  return '未知错误'
}

function normalizeApiError(error: unknown): ApiError {
  if (error instanceof ApiError) return error
  const structured = parseStructuredError(error)
  const nested = parseStructuredError(structured?.error) ?? structured
  const message = rawErrorMessage(nested ?? error)
  const lower = message.toLowerCase()
  const explicitCode = nested?.code ?? nested?.error_code ?? nested?.errorCode
  const explicitMessage = nested?.user_message ?? nested?.userMessage
  const explicitRetryable = nested?.retryable ?? nested?.can_retry ?? nested?.canRetry
  const operationId = nested?.operation_id ?? nested?.operationId

  let code = typeof explicitCode === 'string' ? explicitCode.toUpperCase() : 'OPERATION_FAILED'
  let userMessage = typeof explicitMessage === 'string' ? explicitMessage : message
  let retryable = typeof explicitRetryable === 'boolean' ? explicitRetryable : false

  if (/failed to fetch|networkerror|network request|load failed/.test(lower)) {
    code = 'NETWORK_ERROR'
    userMessage = '无法连接本地服务，请检查应用状态后重试。'
    retryable = true
  } else if (/database is locked|database is busy|resource busy/.test(lower)) {
    code = 'DB_BUSY'
    userMessage = '本地数据库正忙，请稍后重试。'
    retryable = true
  } else if (/not found|no rows|不存在/.test(lower)) {
    code = 'NOT_FOUND'
    userMessage = '目标数据不存在或已更新，请刷新后重试。'
  } else if (/permission|access is denied|拒绝访问/.test(lower)) {
    code = 'PERMISSION_DENIED'
    userMessage = '没有完成此操作所需的文件权限。'
  } else if (/unsupported|暂不支持/.test(lower)) {
    code = 'UNSUPPORTED'
    userMessage = message
  } else if (/invalid|unsupported gesture|must be|不能为空/.test(lower)) {
    code = 'INVALID_REQUEST'
    userMessage = '请求参数无效，请刷新数据后重试。'
  }

  return new ApiError({
    code,
    userMessage,
    retryable,
    operationId: typeof operationId === 'string' ? operationId : undefined,
    cause: error,
  })
}

async function call<T>(operation: string, run: () => Promise<T>): Promise<T> {
  try {
    return await run()
  } catch (error) {
    const parsed = normalizeApiError(error)
    const normalized = parsed.operationId
      ? parsed
      : new ApiError({
          code: parsed.code,
          userMessage: parsed.userMessage,
          retryable: parsed.retryable,
          operationId: nextOperationId(),
          cause: parsed,
        })
    trackError('invoke_error', normalized, {
      operation,
      retryable: normalized.retryable,
      operation_id: normalized.operationId,
    })
    throw normalized
  }
}

async function apiInvoke<T>(operation: string, args?: Record<string, unknown>): Promise<T> {
  return call(operation, () => invoke<T>(operation, args))
}

async function webJson<T>(operation: string, url: string): Promise<T> {
  return call(operation, async () => {
    const response = await fetch(url)
    const text = await response.text()
    let body: unknown = null
    if (text) {
      try {
        body = JSON.parse(text)
      } catch {
        body = text
      }
    }
    if (!response.ok) {
      const record = asRecord(body)
      throw new ApiError({
        code: response.status === 404 ? 'NOT_FOUND' : 'HTTP_ERROR',
        userMessage: rawErrorMessage(record?.error ?? body ?? `HTTP ${response.status}`),
        retryable: response.status >= 500 || response.status === 429,
        operationId: typeof record?.operation_id === 'string' ? record.operation_id : undefined,
      })
    }
    return body as T
  })
}

function unavailable(message: string): never {
  throw new ApiError({ code: 'UNSUPPORTED', userMessage: message, retryable: false })
}

export function errorMessage(error: unknown): string {
  return normalizeApiError(error).userMessage
}

export async function findComfySources(): Promise<FoundSourceDto[]> {
  if (isWebDev()) {
    return webJson<SourceRow[]>('find_comfy_sources', '/api/sources')
      .then((sources) => sources.map((s) => ({ path: s.path, kind: s.kind, origin: '本地数据库' })))
  }
  return apiInvoke('find_comfy_sources')
}

export async function findWorkflowTemplates(): Promise<WorkflowTemplateDto[]> {
  if (isWebDev()) return webJson('find_workflow_templates', '/api/workflow-templates')
  return apiInvoke('find_workflow_templates')
}

export async function refreshWorkflowTemplates(): Promise<number> {
  if (isWebDev()) {
    return webJson<unknown[]>('refresh_workflow_templates', '/api/workflow-templates').then((rows) => rows.length)
  }
  return apiInvoke('refresh_workflow_templates')
}

export async function syncAll(): Promise<SyncAllResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持自动同步，请使用桌面版')
  return apiInvoke('sync_all')
}

export async function addSourceAndScan(path: string, kind: string, alias?: string): Promise<ScanResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持扫描，请使用桌面版执行扫描')
  return apiInvoke('add_source_and_scan', { args: { path, kind, alias: alias ?? null } })
}

export async function listSources(): Promise<SourceRow[]> {
  if (isWebDev()) return webJson('list_sources', '/api/sources')
  return apiInvoke('list_sources')
}

export async function listGroups(sourceId?: number, level: Granularity = 3): Promise<GroupInfo[]> {
  if (isWebDev()) return webJson('list_groups', `/api/groups?sourceId=${sourceId ?? ''}&level=${level}`)
  return apiInvoke('list_groups', { sourceId: sourceId ?? null, level })
}

export async function listGroupImages(groupKey: string, level: Granularity = 3): Promise<ImageRow[]> {
  if (isWebDev()) {
    return webJson('list_group_images', `/api/group-images?groupKey=${encodeURIComponent(groupKey)}&level=${level}`)
  }
  return apiInvoke('list_group_images', { groupKey, level })
}

export async function listGroupImagesAll(groupKey: string, level: Granularity = 3): Promise<ImageRow[]> {
  if (isWebDev()) {
    return webJson('list_group_images_all', `/api/group-images?groupKey=${encodeURIComponent(groupKey)}&level=${level}&includeHidden=1`)
  }
  return apiInvoke('list_group_images_all', { groupKey, level })
}

export async function applySwipeAction(
  imageId: number,
  gesture: string,
  labelId?: number,
  context: ActionContext = {},
): Promise<ActionResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持评分写入，请使用桌面版')
  return apiInvoke('apply_swipe_action', {
    imageId,
    gesture,
    labelId: labelId ?? null,
    sessionId: context.sessionId ?? null,
    startedAt: context.startedAt ?? null,
    contextSignature: context.contextSignature ?? null,
  })
}

export async function toggleHiddenAction(
  imageId: number,
  hidden: boolean,
  context: ActionContext = {},
): Promise<ActionResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持修改屏蔽状态，请使用桌面版')
  return apiInvoke('toggle_hidden_action', {
    imageId,
    hidden,
    sessionId: context.sessionId ?? null,
    startedAt: context.startedAt ?? null,
    contextSignature: context.contextSignature ?? null,
  })
}

export async function undoReviewAction(
  actionId: string,
  sessionId?: string,
): Promise<UndoActionResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持撤销，请使用桌面版')
  return apiInvoke('undo_review_action', {
    actionId,
    sessionId: sessionId ?? null,
  })
}

export async function toggleHidden(imageId: number, hidden: boolean): Promise<void> {
  await toggleHiddenAction(imageId, hidden)
}

export async function trashImage(imageId: number): Promise<void> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持删除文件，请使用桌面版')
  return apiInvoke('trash_image', { imageId })
}

export async function recommendPrompts(
  groupKey: string,
  granularity: number,
  offset: number,
  limit: number,
): Promise<PromptRecommendation[]> {
  if (isWebDev()) {
    return webJson('recommend_prompts', `/api/recommend-prompts?groupKey=${encodeURIComponent(groupKey)}&level=${granularity}&offset=${offset}&limit=${limit}`)
  }
  return apiInvoke('recommend_prompts', { groupKey, granularity, offset, limit })
}

export async function reclusterSource(sourceId: number, threshold: number): Promise<void> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持重新聚类，请使用桌面版')
  return apiInvoke('recluster_source', { sourceId, threshold })
}

export async function mergeGroups(level: number, fromKeys: string[]): Promise<ManualGroupResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持手动合并分组，请使用桌面版')
  return apiInvoke('merge_groups', { args: { level, from_keys: fromKeys } })
}

export async function splitImages(level: number, imageIds: number[]): Promise<ManualGroupResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持手动拆组，请使用桌面版')
  return apiInvoke('split_images', { args: { level, image_ids: imageIds } })
}

export async function listLabels(): Promise<LabelRow[]> {
  if (isWebDev()) return webJson('list_labels', '/api/labels')
  return apiInvoke('list_labels')
}

export async function upsertLabel(id: number | null, name: string, gesture: string, color?: string): Promise<number> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持编辑标签，请使用桌面版')
  return apiInvoke('upsert_label', { input: { id, name, gesture, color: color ?? null } })
}

export async function deleteLabel(id: number): Promise<void> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持编辑标签，请使用桌面版')
  return apiInvoke('delete_label', { id })
}

export async function setImageLabel(imageId: number, labelId: number, on: boolean): Promise<void> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持编辑标签，请使用桌面版')
  return apiInvoke('set_image_label', { imageId, labelId, on })
}

export async function swipe(imageId: number, gesture: string): Promise<number> {
  return applySwipeAction(imageId, gesture).then((result) => result.score)
}

export async function arenaVote(
  groupKey: string,
  left: number,
  right: number,
  winnerIsLeft: boolean,
  context: ActionContext = {},
): Promise<[number, number]> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持擂台评分写入，请使用桌面版')
  return apiInvoke('arena_vote', {
    args: {
      group_key: groupKey,
      left,
      right,
      winner_is_left: winnerIsLeft,
      session_id: context.sessionId ?? null,
      started_at: context.startedAt ?? null,
      context_signature: context.contextSignature ?? null,
    },
  })
}

export async function arenaSuggested(left: number, right: number): Promise<boolean> {
  if (isWebDev()) return webJson('arena_suggested', `/api/arena-suggested?left=${left}&right=${right}`)
  return apiInvoke('arena_suggested', { left, right })
}

export async function exportData(sourceId: number | null, format: 'csv' | 'json', dest: string): Promise<number> {
  return apiInvoke('export_data', { args: { source_id: sourceId, format, dest } })
}

export async function archiveCopy(
  imageIds: number[],
  destDir: string,
  organize: 'flat' | 'label' | 'checkpoint' = 'flat',
): Promise<number> {
  return apiInvoke('archive_copy', { args: { image_ids: imageIds, dest_dir: destDir, organize } })
}

export async function pickFolder(): Promise<string | null> {
  if (isWebDev()) return window.prompt('输入本地图片目录路径')
  const selected = await open({ directory: true, multiple: false })
  return typeof selected === 'string' ? selected : null
}

export async function pickSavePath(defaultName: string): Promise<string | null> {
  const isJson = defaultName.endsWith('.json')
  const selected = await save({
    defaultPath: defaultName,
    filters: isJson
      ? [{ name: 'JSON', extensions: ['json'] }]
      : [{ name: 'CSV', extensions: ['csv'] }],
  })
  return typeof selected === 'string' ? selected : null
}

export async function confirmAction(message: string, detail?: string): Promise<boolean> {
  if (isWebDev()) return window.confirm(detail ? `${message}\n\n${detail}` : message)
  return confirm(detail ? `${message}\n\n${detail}` : message, {
    title: '确认操作',
    kind: 'warning',
    okLabel: '确认',
    cancelLabel: '取消',
  })
}

export function assetUrl(absPath: string): string {
  if (isWebDev()) return `/api/file?path=${encodeURIComponent(absPath)}`
  return convertFileSrc(absPath)
}

export async function getGroupThumbnails(groupKeys: string[], level: Granularity = 3): Promise<GroupThumbDto[]> {
  if (isWebDev()) {
    return webJson('get_group_thumbnails', `/api/group-thumbnails?keys=${encodeURIComponent(groupKeys.join(','))}&level=${level}`)
  }
  return apiInvoke('get_group_thumbnails', { groupKeys, level })
}
