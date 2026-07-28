import { useEffect } from 'react'
import { useStore } from './store'
import { listSources } from './api'
import { ImportPanel } from './components/ImportPanel'
import { GroupList } from './components/GroupList'
import { SwipeDeck } from './components/SwipeDeck'
import { Arena } from './components/Arena'
import { Settings } from './components/Settings'

export default function App() {
  const view = useStore((s) => s.view)
  const setSources = useStore((s) => s.setSources)
  const setView = useStore((s) => s.setView)
  const currentSourceId = useStore((s) => s.currentSourceId)

  // One-shot bootstrap: re-hydrate the registered sources list and, if we
  // still have a valid persisted currentSourceId, land on the groups view
  // instead of the ImportPanel — so a relaunch no longer *looks* like all
  // records vanished.
  useEffect(() => {
    let cancelled = false
    listSources()
      .then((srcs) => {
        if (cancelled) return
        setSources(srcs)
        if (
          currentSourceId != null &&
          srcs.some((s) => s.id === currentSourceId)
        ) {
          setView('groups')
        }
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  return (
    <div className="app">
      {view === 'import' && <ImportPanel />}
      {view === 'groups' && <GroupList />}
      {view === 'swipe' && <SwipeDeck />}
      {view === 'arena' && <Arena />}
      {view === 'settings' && <Settings />}
    </div>
  )
}
