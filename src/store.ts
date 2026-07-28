import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { GroupInfo, Granularity, LabelRow, SourceRow } from './types'

export type View = 'import' | 'groups' | 'swipe' | 'arena' | 'settings'

interface AppState {
  view: View
  sources: SourceRow[]
  currentSourceId: number | null
  groups: GroupInfo[]
  currentGroupKey: string | null
  granularity: Granularity
  labels: LabelRow[]
  setView: (v: View) => void
  setSources: (s: SourceRow[]) => void
  setCurrentSourceId: (id: number | null) => void
  setGroups: (g: GroupInfo[]) => void
  setCurrentGroupKey: (key: string | null) => void
  setGranularity: (g: Granularity) => void
  setLabels: (l: LabelRow[]) => void
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
      setView: (v) => set({ view: v }),
      setSources: (s) => set({ sources: s }),
      setCurrentSourceId: (id) => set({ currentSourceId: id }),
      setGroups: (g) => set({ groups: g }),
      setCurrentGroupKey: (key) => set({ currentGroupKey: key }),
      setGranularity: (g) => set({ granularity: g }),
      setLabels: (l) => set({ labels: l }),
    }),
    {
      name: 'ai-image-sorter-ui',
      partialize: (s) => ({
        currentSourceId: s.currentSourceId,
        granularity: s.granularity,
      }),
    },
  ),
)