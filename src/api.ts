import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import { confirm, open, save } from '@tauri-apps/plugin-dialog'
import type {
  FoundSourceDto,
  Granularity,
  GroupInfo,
  GroupThumbDto,
  ImageRow,
  LabelRow,
  ManualGroupResult,
  ScanResult,
  SourceRow,
  SyncAllResult,
  WorkflowTemplateDto,
} from './types'

export const isWebDev = () => typeof window !== 'undefined' && !('__TAURI_INTERNALS__' in window)

async function webJson<T>(url: string): Promise<T> {
  const response = await fetch(url)
  if (!response.ok) throw new Error(await response.text())
  return response.json() as Promise<T>
}

export async function findComfySources(): Promise<FoundSourceDto[]> {
  if (isWebDev()) return webJson<SourceRow[]>('/api/sources').then((sources) => sources.map((s) => ({ path: s.path, kind: s.kind, origin: '本地数据库' })))
  return invoke('find_comfy_sources')
}

export async function findWorkflowTemplates(): Promise<WorkflowTemplateDto[]> {
  if (isWebDev()) return webJson('/api/workflow-templates')
  return invoke('find_workflow_templates')
}

/** Refresh the saved-workflow template table from ComfyUI user dirs. */
export async function refreshWorkflowTemplates(): Promise<number> {
  if (isWebDev()) return webJson('/api/workflow-templates').then((rows) => (rows as unknown[]).length)
  return invoke('refresh_workflow_templates')
}

/** Background incremental sync of all sources. Off the main thread;
 *  progress arrives via the `sync-progress` event. No-op in web dev
 *  (the bridge is read-only). */
export async function syncAll(): Promise<SyncAllResult> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持自动同步，请使用桌面版')
  return invoke('sync_all')
}

export async function addSourceAndScan(
  path: string,
  kind: string,
  alias?: string,
): Promise<ScanResult> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持扫描，请使用桌面版执行扫描')
  return invoke('add_source_and_scan', {
    args: { path, kind, alias: alias ?? null },
  })
}

export async function listSources(): Promise<SourceRow[]> {
  if (isWebDev()) return webJson('/api/sources')
  return invoke('list_sources')
}

export async function listGroups(sourceId?: number, level: Granularity = 3): Promise<GroupInfo[]> {
  if (isWebDev()) return webJson(`/api/groups?sourceId=${sourceId ?? ''}&level=${level}`)
  return invoke('list_groups', { sourceId: sourceId ?? null, level })
}

export async function listGroupImages(groupKey: string, level: Granularity = 3): Promise<ImageRow[]> {
  if (isWebDev()) return webJson(`/api/group-images?groupKey=${encodeURIComponent(groupKey)}&level=${level}`)
  return invoke('list_group_images', { groupKey, level })
}

/** List all images in a group INCLUDING hidden ones — used by FolderView. */
export async function listGroupImagesAll(
  groupKey: string,
  level: Granularity = 3,
): Promise<ImageRow[]> {
  if (isWebDev()) return webJson(`/api/group-images?groupKey=${encodeURIComponent(groupKey)}&level=${level}&includeHidden=1`)
  return invoke('list_group_images_all', { groupKey, level })
}

/** Toggle whether an image participates in swipe/arena scoring.
 *  Hidden images still appear in FolderView with a gray overlay. */
export async function toggleHidden(imageId: number, hidden: boolean): Promise<void> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持修改屏蔽状态，请使用桌面版')
  return invoke('toggle_hidden', { imageId, hidden })
}

/** Send an image to the OS recycle bin and drop its DB row.
 *  Reversible from the desktop Recycle Bin. */
export async function trashImage(imageId: number): Promise<void> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持删除文件，请使用桌面版')
  return invoke('trash_image', { imageId })
}

/** Recommend high-scoring prompts within a group. Only available at
 *  granularity 0/1/2 — L3 has a single prompt per group. */
export async function recommendPrompts(
  groupKey: string,
  granularity: number,
  offset: number,
  limit: number,
): Promise<import('./types').PromptRecommendation[]> {
  if (isWebDev()) {
    return webJson(`/api/recommend-prompts?groupKey=${encodeURIComponent(groupKey)}&level=${granularity}&offset=${offset}&limit=${limit}`)
  }
  return invoke('recommend_prompts', { groupKey, granularity, offset, limit })
}

/** Recompute L2 prompt-similarity clusters for a source at an explicit
 *  Jaccard threshold (no re-scan). Used by the Settings slider. */
export async function reclusterSource(sourceId: number, threshold: number): Promise<void> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持重新聚类，请使用桌面版')
  return invoke('recluster_source', { sourceId, threshold })
}

/** Manually merge two+ L2 (Prompt偏差) groups into one. The merged group is
 *  pinned so future re-clustering keeps it together. Desktop-only. */
export async function mergeGroups(
  level: number,
  fromKeys: string[],
): Promise<ManualGroupResult> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持手动合并分组，请使用桌面版')
  return invoke('merge_groups', { args: { level, from_keys: fromKeys } })
}

/** Manually split selected images out of the current L2 group into a new
 *  group (they stay split across future re-clustering). Desktop-only. */
export async function splitImages(
  level: number,
  imageIds: number[],
): Promise<ManualGroupResult> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持手动拆组，请使用桌面版')
  return invoke('split_images', { args: { level, image_ids: imageIds } })
}

export async function listLabels(): Promise<LabelRow[]> {
  if (isWebDev()) return webJson('/api/labels')
  return invoke('list_labels')
}

export async function upsertLabel(
  id: number | null,
  name: string,
  gesture: string,
  color?: string,
): Promise<number> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持编辑标签，请使用桌面版')
  return invoke('upsert_label', { input: { id, name, gesture, color: color ?? null } })
}

export async function deleteLabel(id: number): Promise<void> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持编辑标签，请使用桌面版')
  return invoke('delete_label', { id })
}

export async function setImageLabel(
  imageId: number,
  labelId: number,
  on: boolean,
): Promise<void> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持编辑标签，请使用桌面版')
  return invoke('set_image_label', { imageId, labelId, on })
}

export async function swipe(imageId: number, gesture: string): Promise<number> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持评分写入，请使用桌面版')
  return invoke('swipe', { imageId, gesture })
}

export async function arenaVote(
  groupKey: string,
  left: number,
  right: number,
  winnerIsLeft: boolean,
): Promise<[number, number]> {
  if (isWebDev()) throw new Error('浏览器模式暂不支持擂台评分写入，请使用桌面版')
  return invoke('arena_vote', {
    args: { group_key: groupKey, left, right, winner_is_left: winnerIsLeft },
  })
}

export async function arenaSuggested(left: number, right: number): Promise<boolean> {
  if (isWebDev()) return webJson(`/api/arena-suggested?left=${left}&right=${right}`)
  return invoke('arena_suggested', { left, right })
}

export async function exportData(
  sourceId: number | null,
  format: 'csv' | 'json',
  dest: string,
): Promise<number> {
  return invoke('export_data', {
    args: { source_id: sourceId, format, dest },
  })
}

export async function archiveCopy(
  imageIds: number[],
  destDir: string,
  organize?: 'flat' | 'label' | 'checkpoint',
): Promise<number> {
  return invoke('archive_copy', {
    args: { image_ids: imageIds, dest_dir: destDir, organize: organize ?? 'flat' },
  })
}

export async function pickFolder(): Promise<string | null> {
  if (isWebDev()) {
    return window.prompt('输入本地图片目录路径')
  }
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

/** Native confirmation dialog (web dev falls back to window.confirm). */
export async function confirmAction(
  message: string,
  detail?: string,
): Promise<boolean> {
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

export async function getGroupThumbnails(
  groupKeys: string[],
  level: Granularity = 3,
): Promise<GroupThumbDto[]> {
  if (isWebDev()) return webJson(`/api/group-thumbnails?keys=${encodeURIComponent(groupKeys.join(','))}&level=${level}`)
  return invoke('get_group_thumbnails', { groupKeys, level })
}
