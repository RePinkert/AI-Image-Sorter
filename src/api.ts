import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import { confirm, open, save } from '@tauri-apps/plugin-dialog'
import { trackError } from './telemetry'
import type {
  ActionResult,
  AppCapabilitiesDto,
  ArenaHideResult,
  FoundSourceDto,
  Granularity,
  GroupInfo,
  GroupThumbDto,
  ImageRow,
  L2Mode,
  LabelRow,
  ManualGroupResult,
  ModelDeviationDimension,
  MoveSuggestion,
  OutlierInfo,
  PromptRecommendation,
  ScanResult,
  SignatureResultDto,
  SourceRow,
  SyncAllResult,
  TextCoverageDto,
  TextStatusDto,
  UndoActionResult,
  UpdateImagePromptsResult,
  VisionStatusDto,
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
  // The capture channel (?ph=1) must reach EVERY bridge call, not only the
  // image URLs: the bridge masks the prompt text on the way out, and a call
  // that misses the flag would put a real prompt into a screenshot or a probe
  // report. One place decides it, so no call site can forget.
  const target = placeholderMode() && !/[?&]ph=/.test(url)
    ? `${url}${url.includes('?') ? '&' : '?'}ph=1`
    : url
  return call(operation, async () => {
    const response = await fetch(target)
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

export async function updateImagePrompts(image: ImageRow, promptPos: string, promptNeg: string): Promise<UpdateImagePromptsResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持写回图片元数据，请使用桌面版')
  return apiInvoke('update_image_prompts', {
    args: {
      image_id: image.id,
      prompt_pos: promptPos,
      prompt_neg: promptNeg,
      expected_size: image.size,
      expected_modified_at: image.modified_at,
    },
  })
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

/** Batch show/hide every member of an L2 group. Returns changed count. */
export async function hideGroupImages(sourceId: number, groupKey: string, hidden: boolean): Promise<number> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持批量屏蔽，请使用桌面版')
  return apiInvoke('hide_group_images', { sourceId, groupKey, hidden })
}

/** Shift+Delete on a whole L2 group: recycle-bin + DB row per member. */
export async function trashGroupImages(sourceId: number, groupKey: string): Promise<number> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持批量删除，请使用桌面版')
  return apiInvoke('trash_group_images', { sourceId, groupKey })
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

export async function reclusterSource(
  sourceId: number,
  threshold: number,
  mode: L2Mode = 'text',
  visualThreshold = 0.75,
  semanticThreshold = 0.7,
): Promise<void> {
  if (isWebDev()) return unavailable('该模式仅支持桌面端（请使用正式版）')
  return apiInvoke('recluster_source', {
    sourceId,
    threshold,
    mode,
    visualThreshold,
    semanticThreshold,
  })
}

/** Wheel-driven L1 (Model偏差) re-clustering: persist the single selected
 *  dimension, re-derive the source's L1/L3 fingerprints from stored raw
 *  columns, re-cluster L2, and return the new group count. */
export async function reclusterL1(sourceId: number, dimension: ModelDeviationDimension): Promise<number> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持模型偏差重聚类，请使用桌面版')
  return apiInvoke('recluster_l1', { sourceId, dimension })
}

export async function mergeGroups(
  level: number,
  fromKeys: string[],
  sourceId: number | null,
): Promise<ManualGroupResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持手动合并分组，请使用桌面版')
  return apiInvoke('merge_groups', { args: { level, from_keys: fromKeys, source_id: sourceId } })
}

export async function listMoveTargets(sourceId: number, currentL2: string): Promise<GroupInfo[]> {
  if (isWebDev()) {
    // 只读查询 → dev bridge 能提供，这样「移动至其他组」对话框在浏览器里
    // 也能被探针驱动（写操作仍然是桌面端专属）。
    return webJson(
      'list_move_targets',
      `/api/move-targets?sourceId=${sourceId}&currentL2=${encodeURIComponent(currentL2)}`,
    )
  }
  return apiInvoke('list_move_targets', { sourceId, currentL2 })
}

export async function moveImagesToGroup(imageIds: number[], targetGroupKey: string): Promise<ManualGroupResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持移动分组')
  return apiInvoke('move_images_to_group', { args: { image_ids: imageIds, target_group_key: targetGroupKey } })
}

export async function undoSplit(groupKey: string, sourceId: number): Promise<number> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持撤销拆组')
  return apiInvoke('undo_split', { groupKey, sourceId })
}

export async function unmergeGroup(
  groupKey: string,
  sourceId: number | null,
): Promise<number> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持撤销合并，请使用桌面版')
  return apiInvoke('unmerge_group', { args: { group_key: groupKey, source_id: sourceId } })
}

export async function arenaHide(
  groupKey: string,
  survivorId: number,
  victimId: number,
  context: ActionContext = {},
): Promise<ArenaHideResult> {
  if (isWebDev()) return unavailable('浏览器模式暂不支持评分写入，请使用桌面版')
  return apiInvoke('arena_hide', {
    args: {
      group_key: groupKey,
      survivor_id: survivorId,
      victim_id: victimId,
      session_id: context.sessionId ?? null,
      started_at: context.startedAt ?? null,
      context_signature: context.contextSignature ?? null,
    },
  })
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

// Placeholder mode. With `?ph=1` on the page URL the dev bridge answers
// every image request with a deterministic SVG stand-in instead of the
// original bytes, so the screenshot instrument never embeds the user's own
// generated images. Desktop (Tauri) is unaffected: it reads the real file
// through the asset protocol.
export function placeholderMode(): boolean {
  if (typeof window === 'undefined') return false
  try {
    return new URLSearchParams(window.location.search).get('ph') === '1'
  } catch {
    return false
  }
}

export function assetUrl(absPath: string): string {
  if (isWebDev()) {
    const ph = placeholderMode() ? '&ph=1' : ''
    return `/api/file?path=${encodeURIComponent(absPath)}${ph}`
  }
  return convertFileSrc(absPath)
}

export async function getGroupThumbnails(groupKeys: string[], level: Granularity = 3): Promise<GroupThumbDto[]> {
  if (isWebDev()) {
    return webJson('get_group_thumbnails', `/api/group-thumbnails?keys=${encodeURIComponent(groupKeys.join(','))}&level=${level}`)
  }
  return apiInvoke('get_group_thumbnails', { groupKeys, level })
}

// ---------------------------------------------------------------------------
// 识图（阶段六）：本地视觉签名
// ---------------------------------------------------------------------------

/** 「识图」是否可用（视觉模型权重是否就位）。 */
export async function visionStatus(): Promise<VisionStatusDto> {
  if (isWebDev()) return unavailable('识图只在桌面端可用')
  return apiInvoke('vision_status', {})
}

/** 给一个源的所有图片算视觉签名。逐批回传进度（signature-progress 事件）。 */
export async function computeSignatures(sourceId: number, force = false): Promise<SignatureResultDto> {
  if (isWebDev()) return unavailable('识图只在桌面端可用')
  return apiInvoke('compute_signatures', { sourceId, force })
}

/** 取消正在跑的签名批处理。 */
export async function cancelSignatures(): Promise<boolean> {
  if (isWebDev()) return unavailable('识图只在桌面端可用')
  return apiInvoke('cancel_signatures', {})
}

/**
 * 「可能相关」推荐：给定选中的图，哪个组最可能该收留它们。
 * 有视觉签名时按视觉余弦排，没有时退回 prompt 的词 Jaccard。
 */
export async function suggestMoveTargets(
  sourceId: number,
  imageIds: number[],
  currentL2: string,
): Promise<MoveSuggestion[]> {
  if (isWebDev()) {
    const ids = imageIds.join(',')
    return webJson(
      'suggest_move_targets',
      `/api/suggest-move-targets?sourceId=${sourceId}&currentL2=${encodeURIComponent(currentL2)}&imageIds=${ids}`,
    )
  }
  return apiInvoke('suggest_move_targets', { sourceId, imageIds, currentL2 })
}

/** 组内「疑似错分」的图片：组里没有视觉上够格的伙伴的那些。 */
export async function groupOutliers(
  groupKey: string,
  threshold?: number,
): Promise<OutlierInfo[]> {
  if (isWebDev()) {
    const t = threshold ?? 0.75
    return webJson(
      'group_outliers',
      `/api/group-outliers?groupKey=${encodeURIComponent(groupKey)}&threshold=${t}`,
    )
  }
  return apiInvoke('group_outliers', { groupKey, threshold })
}

/**
 * 构建能力自述：这个 exe 里实际带了哪些能力。
 *
 * 发行版 EXE 是用户主动审查的产物，所以「这一份有什么」必须能被直接看见，
 * 而不是靠文件大小 / 时间戳去猜。标记串由**实现模块**生成，所以这份清单
 * 不可能声称一个没实现的能力。
 */
export async function appCapabilities(): Promise<AppCapabilitiesDto> {
  if (isWebDev()) return unavailable('构建能力自述只在桌面端可用')
  return apiInvoke('app_capabilities', {})
}

// ---------------------------------------------------------------------------
// 文本塔（阶段七轮 1）：prompt 语义向量
// ---------------------------------------------------------------------------

/** 文本塔是否可用（权重与分词器都在才算）。 */
export async function textStatus(): Promise<TextStatusDto> {
  if (isWebDev()) return unavailable('文本塔只在桌面端可用')
  return apiInvoke('text_status', {})
}

/** 该源的文本塔覆盖率（已算 / 有 prompt 的总数）。 */
export async function textCoverage(sourceId: number): Promise<TextCoverageDto> {
  if (isWebDev()) return unavailable('文本塔只在桌面端可用')
  return apiInvoke('text_coverage', { sourceId })
}

/**
 * 给一个源的所有 prompt 算文本签名。
 *
 * 与视觉塔不同：输入是 prompt 文本（不读图），所以快得多；批大小恒为 8
 * （int8 量化权重对批大小敏感，与实验室同口径）。
 */
export async function computeTextSignatures(sourceId: number, force = false): Promise<SignatureResultDto> {
  if (isWebDev()) return unavailable('文本塔只在桌面端可用')
  return apiInvoke('compute_text_signatures', { sourceId, force })
}

/** 取消正在跑的文本签名批处理。 */
export async function cancelTextSignatures(): Promise<boolean> {
  if (isWebDev()) return unavailable('文本塔只在桌面端可用')
  return apiInvoke('cancel_text_signatures', {})
}
