export interface SourceRow {
  id: number
  path: string
  kind: string
  alias: string | null
  scanned_at: string | null
}

export interface LabelRow {
  id: number
  name: string
  gesture: string
  color: string | null
}

export interface ImageRow {
  id: number
  source_id: number
  abs_path: string
  filename: string
  width: number
  height: number
  prompt_pos: string
  prompt_neg: string
  checkpoint: string
  loras: string
  vae: string
  samplers: string
  seed: number
  meta_ok: boolean
  source_kind: string
  score: number | null
  labels: LabelRow[]
}

export type Granularity = 0 | 1 | 2 | 3

export interface GroupInfo {
  group_key: string
  count: number
  prompt_pos: string
  checkpoint: string
  source_kind: string
  source_path: string
}

export interface FoundSourceDto {
  path: string
  kind: string
  origin: string
}

export interface ScanResult {
  source_id: number
  scanned: number
  groups: number
}

export interface GroupThumbDto {
  group_key: string
  thumb_path: string
}
