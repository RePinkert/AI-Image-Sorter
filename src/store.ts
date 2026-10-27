import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { GroupInfo, Granularity, L2Mode, LabelRow, SettingsSection, SourceRow, SyncProgress } from './types'
import { DEFAULT_KEYMAP, type Binding, type Keymap } from './keymap'
import { invalidateGroupCaches } from './lib/groupCache'

export type View = 'import' | 'groups' | 'swipe' | 'arena' | 'folder' | 'settings'

export interface SyncProgressState {
  active: boolean
  stage: string
  sourceIndex: number
  sourceTotal: number
  sourcePath: string
  found: number
  processed: number
  added: number
  pending: number
  parseErrors: number
}

export interface ReviewSession {
  mode: 'swipe' | 'arena'
  groupKey: string | null
  granularity: Granularity
  swipeOrder: number[]
  swipeCursor: number
  swipeUndoStack: Array<{
    actionId: string
    imageId: number
    index: number
    kind: 'swipe' | 'hide'
  }>
  arenaPair: [number, number] | null
  arenaLastHideActionId: string | null
  arenaLastHiddenImageId: number | null
  /** The pair that was on screen when the last arena hide happened — undo
   *  restores exactly this pair so the user lands back where they were. */
  arenaLastHidePair: [number, number] | null
}

const EMPTY_REVIEW_SESSION: ReviewSession = {
  mode: 'swipe',
  groupKey: null,
  granularity: 3,
  swipeOrder: [],
  swipeCursor: 0,
  swipeUndoStack: [],
  arenaPair: null,
  arenaLastHideActionId: null,
  arenaLastHiddenImageId: null,
  arenaLastHidePair: null,
}

export const IDLE_SYNC_PROGRESS: SyncProgressState = {
  active: false,
  stage: '',
  sourceIndex: 0,
  sourceTotal: 0,
  sourcePath: '',
  found: 0,
  processed: 0,
  added: 0,
  pending: 0,
  parseErrors: 0,
}

interface AppState {
  view: View
  sources: SourceRow[]
  currentSourceId: number | null
  groups: GroupInfo[]
  currentGroupKey: string | null
  granularity: Granularity
  labels: LabelRow[]
  // L2 prompt-similarity Jaccard threshold — surfaced as a Settings slider
  // and persisted across launches. Default 0.3 matches
  // clustering.rs::DEFAULT_L2_THRESHOLD. The lower default is needed
  // because AI prompts share a long common prefix (quality tags, style
  // keywords) and only differ in a short varying suffix — Jaccard on
  // raw tokens sees high overlap even for visually distinct outputs.
  // The recluster_source command applies the user's choice live.
  l2Threshold: number
  /** L2 聚类方式（阶段六）：
   *  - text     词 Jaccard（历史行为）
   *  - visual  视觉塔向量 + 平均连接（实测追平并超过人工合并）
   *  - semantic 文本塔向量 + 平均连接（prompt 语义；便宜 100 倍，不读图）
   *
   * 默认仍是 `text` —— 代码实际行为与本注释必须一致（档 0 修订时发现文档
   * 与代码曾经各说各话）。新档位需显式选择。 */
  l2Mode: L2Mode
  /** 视觉模式（CLIP）的余弦阈值。0.75 是实测标定出来的 CLIP 值。 */
  l2VisualThreshold: number
  /** 语义模式（BGE）的余弦阈值。**与视觉档分开** —— 0.75 是 CLIP 的值，
   *  绝不可沿用到 BGE（量纲不同）。默认值 0.7 是**暂定值、未标定**
   *  （阶段七轮 3 走双源标定）。 */
  l2SemanticThreshold: number
  // Persisted keyboard shortcuts. Defaults match the historical hardcoded
  // keys; the arena hide action moved to the Shift combo (hold Shift to arm,
  // Shift+←/→ to hide the matching card).
  keybindings: Keymap
  syncStatus: 'idle' | 'syncing' | 'success' | 'error'
  syncMessage: string
  syncUpdatedAt: string | null
  syncProgress: SyncProgressState
  reviewSession: ReviewSession
  dataRevision: number
  /** L2 (Prompt偏差) list-window presentation: cards (legacy) or expandable
   *  rows. Cards stay the default so the rows experiment can be switched
   *  off instantly if it regresses. */
  l2ListView: boolean
  setL2ListView: (on: boolean) => void
  /** 跨页联动：设置页要聚焦到哪一节。
   *
   *  从别的页面跳过来时带上（移动对话框的「模型建议」栏会声明
   *  「这列数字依赖 L2 聚类方式」并给一个「去调整」按钮）。null = 不聚焦。
   *  **不持久化** —— 它是一次性的落点，不是偏好。 */
  settingsFocus: SettingsSection | null
  setSettingsFocus: (s: SettingsSection | null) => void
  setView: (v: View) => void
  setSources: (s: SourceRow[]) => void
  setCurrentSourceId: (id: number | null) => void
  setGroups: (g: GroupInfo[]) => void
  setCurrentGroupKey: (key: string | null) => void
  setGranularity: (g: Granularity) => void
  setLabels: (l: LabelRow[]) => void
  setL2Threshold: (t: number) => void
  setL2Mode: (m: L2Mode) => void
  setL2VisualThreshold: (t: number) => void
  setL2SemanticThreshold: (t: number) => void
  setKeybinding: (action: keyof Keymap, binding: Binding) => void
  resetKeybindings: () => void
  setSyncState: (status: AppState['syncStatus'], message: string) => void
  applySyncProgress: (p: SyncProgress) => void
  resetSyncProgress: () => void
  updateReviewSession: (update: Partial<ReviewSession>) => void
  bumpDataRevision: () => void
  /** 组**成员**变化的代数。缩略图 / 行内图片条据此判断缓存是否过期 ——
   *  与 dataRevision 分开：后者连「评了一张图」都会涨，用它重取缩略图
   *  太浪费（同步每 8 秒跑一次）。 */
  groupRevision: number
  bumpGroupRevision: () => void
}

// Persist only UI/display state, not the bulky fetched data, so a relaunch
// lands the user back on their groups view instead of the ImportPanel.
export const useStore = create<AppState>()(
  persist(
    (set) => ({
      view: 'import',
      sources: [],
      currentSourceId: null,
      groups: [],
      currentGroupKey: null,
      granularity: 3,
      labels: [],
      l2Threshold: 0.3,
      l2Mode: 'text',
      l2VisualThreshold: 0.75,
      l2SemanticThreshold: 0.7,
      keybindings: DEFAULT_KEYMAP,
      syncStatus: 'idle',
      syncMessage: '',
      syncUpdatedAt: null,
      syncProgress: IDLE_SYNC_PROGRESS,
      reviewSession: EMPTY_REVIEW_SESSION,
      dataRevision: 0,
      groupRevision: 0,
      l2ListView: false,
      setL2ListView: (on) => set({ l2ListView: on }),
      settingsFocus: null,
      setSettingsFocus: (s) => set({ settingsFocus: s }),
      setView: (v) => set((s) => ({
        view: v,
        reviewSession: v === 'swipe' || v === 'arena'
          ? { ...s.reviewSession, mode: v }
          : s.reviewSession,
      })),
      setSources: (s) => set({ sources: s }),
      setCurrentSourceId: (id) => set({ currentSourceId: id }),
      setGroups: (g) => set({ groups: g }),
      setCurrentGroupKey: (key) => set((s) => ({
        currentGroupKey: key,
        reviewSession: s.reviewSession.groupKey === key
          ? s.reviewSession
          : { ...EMPTY_REVIEW_SESSION, groupKey: key, granularity: s.granularity },
      })),
      setGranularity: (g) => set((s) => ({
        granularity: g,
        currentGroupKey: s.granularity === g ? s.currentGroupKey : null,
        reviewSession: s.reviewSession.granularity === g
          ? s.reviewSession
          : { ...EMPTY_REVIEW_SESSION, granularity: g },
      })),
      setLabels: (l) => set({ labels: l }),
      setL2Threshold: (t) => set({ l2Threshold: t }),
      setL2Mode: (m) => set({ l2Mode: m }),
      setL2VisualThreshold: (t) => set({ l2VisualThreshold: t }),
      setL2SemanticThreshold: (t) => set({ l2SemanticThreshold: t }),
      setKeybinding: (action, binding) =>
        set((s) => ({ keybindings: { ...s.keybindings, [action]: binding } })),
      resetKeybindings: () => set({ keybindings: DEFAULT_KEYMAP }),
      setSyncState: (status, message) => set({ syncStatus: status, syncMessage: message, syncUpdatedAt: new Date().toISOString() }),
      applySyncProgress: (p) =>
        set({
          syncProgress: {
            active: true,
            stage: p.stage,
            sourceIndex: p.source_index,
            sourceTotal: p.source_total,
            sourcePath: p.source_path,
            found: p.found,
            processed: p.processed,
            added: p.added,
            pending: p.pending,
            parseErrors: p.parse_errors,
          },
        }),
      resetSyncProgress: () => set({ syncProgress: { ...IDLE_SYNC_PROGRESS } }),
      updateReviewSession: (update) => set((s) => ({
        reviewSession: { ...s.reviewSession, ...update },
      })),
      bumpDataRevision: () => set((s) => ({ dataRevision: s.dataRevision + 1 })),
      bumpGroupRevision: () => {
        // 先作废缓存再涨代数：缓存是模块级的，作废必须与代数同步发生，
        // 否则「先涨代数后清缓存」之间挂载的组件会拿到旧值。
        invalidateGroupCaches()
        set((s) => ({ groupRevision: s.groupRevision + 1 }))
      },
    }),
    {
      name: 'ai-image-sorter-ui',
      partialize: (s) => ({
        currentSourceId: s.currentSourceId,
        granularity: s.granularity,
        currentGroupKey: s.currentGroupKey,
        l2Threshold: s.l2Threshold,
        l2Mode: s.l2Mode,
        l2VisualThreshold: s.l2VisualThreshold,
    l2SemanticThreshold: s.l2SemanticThreshold,
        keybindings: s.keybindings,
        reviewSession: s.reviewSession,
        l2ListView: s.l2ListView,
      }),
    },
  ),
)
