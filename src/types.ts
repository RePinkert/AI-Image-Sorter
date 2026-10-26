export interface SourceRow {
  id: number
  path: string
  kind: string
  alias: string | null
  scanned_at: string | null
  /** Persisted L2 Jaccard threshold this source was last re-clustered at. */
  l2_threshold?: number
  /** Dimensions currently included in the L1 Model偏差 key. */
  model_deviation_dimensions?: ModelDeviationDimension[]
}

export type ModelDeviationDimension =
  | 'base_model'
  | 'text_encoder'
  | 'vae'
  | 'lora'
  | 'embedding'
  | 'controlnet'

/** Wheel-order for the Model偏差 level: scroll cycles the source's SINGLE
 *  selected dimension in this exact sequence. */
export const MODEL_DEVIATION_DIMENSIONS: ModelDeviationDimension[] = [
  'base_model',
  'text_encoder',
  'vae',
  'lora',
  'embedding',
  'controlnet',
]

export const MODEL_DEVIATION_DIMENSION_LABELS: Record<ModelDeviationDimension, string> = {
  base_model: 'Base Model',
  text_encoder: 'Text Encoder',
  vae: 'VAE',
  lora: 'LoRA',
  embedding: 'Embedding',
  controlnet: 'ControlNet',
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
  group_key_l1: string
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
  /** File modification time as Unix seconds (folder-view sorting). */
  modified_at: number
  /** Primary diffusion model (v8+). Empty for legacy rows until reparse. */
  diffusion_model?: string
  /** Saved-template name this image's workflow matched (if any). */
  workflow_name?: string | null
  /** When the image is pinned to a manual L2 group override, the binding
   *  kind ('merge' | 'split'); null when it follows auto-clustering. */
  manually_grouped?: string | null
}

export interface UpdateImagePromptsResult {
  prompt_pos: string
  prompt_neg: string
  group_key_l2: string
  group_key_l3: string
  modified_at: number
  size: number
}

export type Granularity = 0 | 1 | 2 | 3

/**
 * L2 聚类方式（阶段六）。
 *  - text     词 Jaccard（历史行为；L3 与「本地改 prompt」依赖它）
 *  - visual   视觉塔向量 + 平均连接（默认；实测追平并超过人工合并）
 *  - semantic 文本塔向量 + 平均连接（没有视觉塔时的退路）
 */
export type L2Mode = 'text' | 'visual' | 'semantic'

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
  /** Model-parameter label for the L1 (Model偏差) level under the source's
   *  selected dimension (e.g. the exact base model file name). Empty at
   *  other levels / cross-source view. */
  model_param?: string
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
  parse_errors: number
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
  parse_errors: number
}

export interface SyncAllResult {
  sources: number
  added: number
  pending: number
  reclustered: boolean
  parse_errors: number
}

export interface GroupThumbDto {
  group_key: string
  thumb_paths: string[]
}

/** 「识图」是否可用（视觉模型权重是否就位）。 */
export interface VisionStatusDto {
  available: boolean
  model_id: string
  model_path: string
}

/**
 * 文本塔（prompt 语义）是否可用。
 *
 * 权重与分词器**缺任一份**就整体不可用 —— 所以这里只有一个 `available`，
 * 没有「有模型但没有分词器」这种半可用状态。
 */
export interface TextStatusDto {
  available: boolean
  model_id: string
  model_path: string
  tokenizer_path: string
  /** 分词器词表大小（可读时才有值）。 */
  vocab_size?: number | null
}

/** 文本塔覆盖率（该源有多少条 prompt 已算出向量）。 */
export interface TextCoverageDto {
  /** 有非空 prompt 的图数（分母）。 */
  total: number
  /** 已算出文本签名的图数（分子）。 */
  computed: number
  /** 向量维度（已算过才有值）。 */
  dim?: number | null
}

/**
 * 一条构建能力。
 *
 * `marker` 是**二进制里能被探针扫到的标记串**，它由实现该能力的模块生成
 * （Rust 侧 `capabilities.rs`）—— 所以「声称有但没实现」在二进制层面
 * 就会被 `scripts\verify-release-binary.ps1` 发现。
 */
export interface CapabilityDto {
  id: string
  marker: string
  label: string
  /** 'clustering' | 'tower' | 'provenance' */
  kind: string
  /** 作用层级。本轮只有 L2（Prompt偏差）。 */
  axis: string
}

/** 一个模型塔的就位状态（就位 = 这次运行里真的能用）。 */
export interface TowerCapabilityDto {
  id: string
  label: string
  model_id: string
  available: boolean
}

/** 构建能力自述：这个 exe 里到底有什么。 */
export interface AppCapabilitiesDto {
  version: string
  build_ts: string
  build_id: string
  capabilities: CapabilityDto[]
  l2_modes: string[]
  towers: TowerCapabilityDto[]
}

/** 签名批处理的进度（signature-progress 事件）。 */
export interface SignatureProgressDto {
  slot: string
  done: number
  total: number
}

/** 签名批处理的结果。 */
export interface SignatureResultDto {
  computed: number
  skipped: number
  failed: number
  total: number
  model_id: string
  dim: number
  cancelled: boolean
}

/** 「可能相关」推荐的一条。 */
export interface MoveSuggestion {
  group_key: string
  count: number
  prompt_pos: string
  /** 0..1 的相似度（两种基准的量纲不同，靠 basis 区分）。 */
  score: number
  /**
   * 相似度基准：
   * - 'vision'（视觉余弦，CLIP）
   * - 'semantic'（语义余弦，BGE）
   * - 'text'（词 Jaccard）
   *
   * 三者的量纲**不可比** —— 标错会让用户以为「24% 的视觉相似」其实是语义相似。
   */
  basis: 'vision' | 'semantic' | 'text'
}

/** 组内「疑似错分」的一张图。 */
export interface OutlierInfo {
  image_id: number
  /** 它在组内与其它成员的最大余弦（越低越像「不属于这里」）。 */
  max_similarity: number
  /** 最像的别的组。 */
  best_group_key: string | null
  best_score: number
}

export interface PromptRecommendation {
  prompt_text: string
  prompt_neg: string
  diffusion_model: string
  checkpoint: string
  loras: Array<{
    name: string
    strength: number
    clip_strength?: number
  }>
  vae: string
  sampler: string
  scheduler: string
  steps: number
  cfg: number
  width: number
  height: number
  aspect_ratio: number
  sample_count: number
  max_score: number
  avg_score: number
  median_score: number
  score_variance: number
  confidence: number
  example_image_ids: number[]
  image_count: number
}

export interface ActionResult {
  action_id: string
  image_id: number
  score: number
  hidden: boolean
  label_id: number | null
  committed_at: string
}

export interface UndoActionResult {
  action_id: string
  undone_action_id: string
  image_id: number
  restored_score: number | null
  hidden: boolean
  label_id: number | null
  gesture: string | null
  committed_at: string
}

export interface ManualGroupResult {
  group_key: string
  moved: number
}

/** Result of an arena hide: victim hidden with score 0, survivor credited
 *  like an arena winner (its score moves through the same Bradley-Terry
 *  update as a normal vote). */
export interface ArenaHideResult {
  action_id: string
  survivor_id: number
  survivor_score: number
  victim_id: number
  victim_score: number
}
