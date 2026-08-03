import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { GroupInfo, Granularity, LabelRow, SourceRow, SyncProgress } from './types'
import { DEFAULT_KEYMAP, type Binding, type Keymap } from './keymap'

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
  setView: (v: View) => void
  setSources: (s: SourceRow[]) => void
  setCurrentSourceId: (id: number | null) => void
  setGroups: (g: GroupInfo[]) => void
  setCurrentGroupKey: (key: string | null) => void
  setGranularity: (g: Granularity) => void
  setLabels: (l: LabelRow[]) => void
  setL2Threshold: (t: number) => void
  setKeybinding: (action: keyof Keymap, binding: Binding) => void
  resetKeybindings: () => void
  setSyncState: (status: AppState['syncStatus'], message: string) => void
  applySyncProgress: (p: SyncProgress) => void
  resetSyncProgress: () => void
  updateReviewSession: (update: Partial<ReviewSession>) => void
  bumpDataRevision: () => void
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
      keybindings: DEFAULT_KEYMAP,
      syncStatus: 'idle',
      syncMessage: '',
      syncUpdatedAt: null,
      syncProgress: IDLE_SYNC_PROGRESS,
      reviewSession: EMPTY_REVIEW_SESSION,
      dataRevision: 0,
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
    }),
    {
      name: 'ai-image-sorter-ui',
      partialize: (s) => ({
        currentSourceId: s.currentSourceId,
        granularity: s.granularity,
        currentGroupKey: s.currentGroupKey,
        l2Threshold: s.l2Threshold,
        keybindings: s.keybindings,
        reviewSession: s.reviewSession,
      }),
    },
  ),
)
