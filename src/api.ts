import { convertFileSrc, invoke } from '@tauri-apps/api/core'
import { open, save } from '@tauri-apps/plugin-dialog'
import type {
  FoundSourceDto,
  Granularity,
  GroupInfo,
  GroupThumbDto,
  ImageRow,
  LabelRow,
  ScanResult,
  SourceRow,
} from './types'

export async function findComfySources(): Promise<FoundSourceDto[]> {
  return invoke('find_comfy_sources')
}

export async function addSourceAndScan(
  path: string,
  kind: string,
  alias?: string,
): Promise<ScanResult> {
  return invoke('add_source_and_scan', {
    args: { path, kind, alias: alias ?? null },
  })
}

export async function listSources(): Promise<SourceRow[]> {
  return invoke('list_sources')
}

export async function listGroups(sourceId?: number, level: Granularity = 3): Promise<GroupInfo[]> {
  return invoke('list_groups', { sourceId: sourceId ?? null, level })
}

export async function listGroupImages(groupKey: string, level: Granularity = 3): Promise<ImageRow[]> {
  return invoke('list_group_images', { groupKey, level })
}

export async function listLabels(): Promise<LabelRow[]> {
  return invoke('list_labels')
}

export async function upsertLabel(
  id: number | null,
  name: string,
  gesture: string,
  color?: string,
): Promise<number> {
  return invoke('upsert_label', { input: { id, name, gesture, color: color ?? null } })
}

export async function deleteLabel(id: number): Promise<void> {
  return invoke('delete_label', { id })
}

export async function setImageLabel(
  imageId: number,
  labelId: number,
  on: boolean,
): Promise<void> {
  return invoke('set_image_label', { imageId, labelId, on })
}

export async function swipe(imageId: number, gesture: string): Promise<number> {
  return invoke('swipe', { imageId, gesture })
}

export async function arenaVote(
  groupKey: string,
  left: number,
  right: number,
  winnerIsLeft: boolean,
): Promise<[number, number]> {
  return invoke('arena_vote', {
    args: { group_key: groupKey, left, right, winner_is_left: winnerIsLeft },
  })
}

export async function arenaSuggested(left: number, right: number): Promise<boolean> {
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

export function assetUrl(absPath: string): string {
  return convertFileSrc(absPath)
}

export interface GroupThumbDtoLocal {
  group_key: string
  thumb_path: string
}

export async function getGroupThumbnails(
  groupKeys: string[],
  level: Granularity = 3,
): Promise<GroupThumbDto[]> {
  return invoke('get_group_thumbnails', { groupKeys, level })
}
