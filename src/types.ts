export interface SourceRow {
  id: number
  path: string
  kind: string
  alias: string | null
  scanned_at: string | null
  /** Persisted L2 Jaccard threshold this source was last re-clustered at. */
  l2_threshold?: number
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
  /** Hidden = excluded from swipe/arena scoring; still listed in FolderView. */
  hidden: boolean
  /** File size in bytes (folder-view sorting). */
  size: number
  /** Primary diffusion model (v8+). Empty for legacy rows until reparse. */
  diffusion_model?: string
  /** Saved-template name this image's workflow matched (if any). */
  workflow_name?: string | null
  /** When the image is pinned to a manual L2 group override, the binding
   *  kind ('merge' | 'split'); null when it follows auto-clustering. */
  manually_grouped?: string | null
}

export type Granularity = 0 | 1 | 2 | 3

export interface ModelFacet {
  model: string
  count: number
}

export interface GroupInfo {
  group_key: string
  count: number
  prompt_pos: string
  checkpoint: string
  source_kind: string
  source_path: string
  /** Template name for the workflow level (granularity 1), else null. */
  workflow_name?: string | null
  /** Distinct diffusion models + image counts for the workflow level. */
  model_facets?: ModelFacet[]
  /** True when the group's members are pinned by a manual L2 override. */
  manually_merged?: boolean
}

export interface FoundSourceDto {
  path: string
  kind: string
  origin: string
}

export interface WorkflowTemplateDto {
  name: string
  path: string
  workflow_id: string | null
  topology_signature: string
  graph_json: string
  node_count: number
  diffusion_models: string[]
  model_chain: string[]
}

export interface ScanResult {
  source_id: number
  scanned: number
  groups: number
}

export interface SyncProgress {
  stage: string
  source_index: number
  source_total: number
  source_path: string
  found: number
  processed: number
  added: number
  pending: number
}

export interface SyncAllResult {
  sources: number
  added: number
  pending: number
  reclustered: boolean
}

export interface GroupThumbDto {
  group_key: string
  thumb_paths: string[]
}

export interface PromptRecommendation {
  prompt_text: string
  diffusion_model: string
  max_score: number
  avg_score: number
  image_count: number
}

export interface ManualGroupResult {
  group_key: string
  moved: number
}
