import { useEffect } from 'react'
import { listen } from '@tauri-apps/api/event'
import { useStore } from './store'
import { errorMessage, isWebDev, listGroups, listSources, syncAll } from './api'
import type { SyncProgress } from './types'
import { ImportPanel } from './components/ImportPanel'
import { GroupList } from './components/GroupList'
import { SwipeDeck } from './components/SwipeDeck'
import { Arena } from './components/Arena'
import { FolderView } from './components/FolderView'
import { Settings } from './components/Settings'
import { track, trackDwell, trackView } from './telemetry'

export default function App() {
  const view = useStore((s) => s.view)
  const setSources = useStore((s) => s.setSources)
  const setGroups = useStore((s) => s.setGroups)
  const setCurrentGroupKey = useStore((s) => s.setCurrentGroupKey)
  const setL2Threshold = useStore((s) => s.setL2Threshold)
  const setView = useStore((s) => s.setView)
  const currentSourceId = useStore((s) => s.currentSourceId)
  const currentGroupKey = useStore((s) => s.currentGroupKey)
  const syncStatus = useStore((s) => s.syncStatus)
  const syncMessage = useStore((s) => s.syncMessage)
  const setSyncState = useStore((s) => s.setSyncState)
  const syncProgress = useStore((s) => s.syncProgress)
  const applySyncProgress = useStore((s) => s.applySyncProgress)
  const resetSyncProgress = useStore((s) => s.resetSyncProgress)
  const bumpDataRevision = useStore((s) => s.bumpDataRevision)
  const bumpGroupRevision = useStore((s) => s.bumpGroupRevision)

  useEffect(() => {
    const enteredAt = Date.now()
    trackView(view, 'enter')
    return () => {
      const duration = Date.now() - enteredAt
      trackView(view, 'exit', duration)
      trackDwell(view, duration)
    }
  }, [view])

  // One-shot bootstrap: re-hydrate the registered sources list and, if we
  // still have a valid persisted currentSourceId (and a currentGroupKey),
  // drop straight into the swipe deck of that group rather than the
  // ImportPanel — so a relaunch no longer *looks* like all records
  // vanished, and the user doesn't have to manually re-navigate.
  useEffect(() => {
    let cancelled = false
    listSources()
      .then(async (srcs) => {
        if (cancelled) return
        setSources(srcs)
        if (
          currentSourceId != null &&
          srcs.some((s) => s.id === currentSourceId)
        ) {
          // The persisted per-source threshold is authoritative for the
          // Settings slider / merge suggestions.
          const src = srcs.find((s) => s.id === currentSourceId)
          if (src?.l2_threshold != null) setL2Threshold(src.l2_threshold)
          const groups = await listGroups(currentSourceId, useStore.getState().granularity)
          setGroups(groups)
          const validGroup = currentGroupKey && groups.some((g) => g.group_key === currentGroupKey)
          if (!validGroup) setCurrentGroupKey(null)
          if (cancelled) return
          const session = useStore.getState().reviewSession
          const resumeMode = validGroup && session.groupKey === currentGroupKey
            ? session.mode
            : 'swipe'
          setView(validGroup ? resumeMode : 'groups')
        }
      })
      .catch((error) => setSyncState('error', `初始化失败：${errorMessage(error)}`))
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Live sync progress from the backend worker thread.
  useEffect(() => {
    if (isWebDev()) return
    let disposed = false
    let unlisten: (() => void) | undefined
    listen<SyncProgress>('sync-progress', (e) => {
      if (disposed) return
      const p = e.payload
      applySyncProgress(p)
      if (p.stage === 'scan') {
          setSyncState('syncing', `扫描 ${p.source_index + 1}/${p.source_total}: 已发现 ${p.found} 张 · 已处理 ${p.processed} 张 · 新增 ${p.added} 张${p.pending > 0 ? ` · 待扫描 ${p.pending} 张` : ''}${p.parse_errors > 0 ? ` · 解析失败 ${p.parse_errors} 张` : ''}`)
      } else if (p.stage === 'recluster') {
        setSyncState('syncing', '重新聚类相似分组…')
      } else if (p.stage === 'scan-done') {
        setSyncState('syncing', `${p.source_index + 1}/${p.source_total} 完成: ${p.source_path}`)
      }
    }).then((un) => {
      unlisten = un
    })
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [applySyncProgress, setSyncState])

  // Keep registered folders in sync while the app is open. The scan is
  // idempotent, so existing rows retain their scores and labels. Runs in
  // the background (off the UI thread); progress events drive the bar.
  useEffect(() => {
    if (isWebDev()) return
    let running = false
    let hideTimer: ReturnType<typeof setTimeout> | null = null
    let lastErrorTrackAt = 0
    let lastPending: number | null = null
    let refreshRequest = 0
    const refreshGroups = async () => {
      const state = useStore.getState()
      if (state.currentSourceId == null) return
      const sourceId = state.currentSourceId
      const level = state.granularity
      const request = ++refreshRequest
      const groups = await listGroups(sourceId, level)
      const latest = useStore.getState()
      if (
        request !== refreshRequest ||
        latest.currentSourceId !== sourceId ||
        latest.granularity !== level
      ) return
      setGroups(groups)
      if (
        latest.currentGroupKey &&
        !groups.some((g) => g.group_key === latest.currentGroupKey) &&
        !['swipe', 'arena', 'folder'].includes(latest.view)
      ) {
        setCurrentGroupKey(null)
        if (latest.view !== 'import') setView('groups')
      }
    }
    const sync = async () => {
      if (running) return
      running = true
      const startedAt = Date.now()
      try {
        const result = await syncAll()
        // Only refresh the group list when the sync actually did something.
        // No-op polls (nothing new, nothing pending, no recluster) skip the
        // refetch so the UI doesn't churn every 8 seconds.
        const pendingChanged = result.pending > 0 && result.pending !== lastPending
        lastPending = result.pending
        // 「改名识别」也算真活干（库里换了路径 → 列表要刷新）。
        // 「待确认」**不算** —— 那些记录一直都在，每轮都数一遍会让顶栏一直闪。
        const hadWork =
          result.added > 0 ||
          pendingChanged ||
          result.reclustered ||
          result.parse_errors > 0 ||
          result.renamed > 0
        if (hadWork) {
          const sources = await listSources()
          setSources(sources)
          await refreshGroups()
          bumpDataRevision()
          // 重聚类换了组 → 列表里的缩略图全部作废（同步每 8 秒一次，
          // 所以只在真的重聚类时才作废，不跟着每次同步都刷）。
          if (result.reclustered) bumpGroupRevision()
          track('sync', {
            success: true,
            source_count: result.sources,
            added_count: result.added,
            pending_count: result.pending,
            parse_error_count: result.parse_errors,
            reclustered: result.reclustered,
            renamed_count: result.renamed,
            unconfirmed_count: result.unconfirmed,
            duration_ms: Date.now() - startedAt,
          })
        }
        // No-op polls stay silent; only surface the bar when real work
        // happened, and auto-hide the success message after a moment.
        if (!hadWork) {
          setSyncState('idle', '')
          resetSyncProgress()
        } else {
          // 说什么，取决于**真的发生了什么**（第 30 轮）。
          //
          // 旧版固定带一句「已检查 1 个目录」—— 那不是用户关心的事（他自己
          // 知道库里有几个目录），而且在什么都没发生的时候也照报。军规 05
          // 说的就是这种冗余标签：只说变化。
          const parts: string[] = []
          if (result.added > 0) parts.push(`新增 ${result.added} 张`)
          if (result.pending > 0) parts.push(`待扫描 ${result.pending} 张`)
          if (result.parse_errors > 0) parts.push(`解析失败 ${result.parse_errors} 张`)
          // 改名识别（第 29 轮）必须**说出来** —— 库变了，用户有权知道。
          if (result.renamed > 0) {
            parts.push(`识别出 ${result.renamed} 个改名并已按内容更新记录`)
          }
          if (result.unconfirmed > 0) parts.push(`${result.unconfirmed} 条记录待确认`)
          setSyncState('success', parts.length > 0 ? `同步完成：${parts.join('，')}` : '同步完成')
          // Once nothing is left pending the stale progress would otherwise
          // keep the "还有 X 张未扫描" warning alive forever on no-op polls.
          if (result.pending === 0) resetSyncProgress()
          if (hideTimer) clearTimeout(hideTimer)
          hideTimer = setTimeout(() => {
            if (useStore.getState().syncStatus === 'success') setSyncState('idle', '')
          }, 3000)
        }
      } catch (error) {
        setSyncState('error', `同步失败：${errorMessage(error)}`)
        const now = Date.now()
        if (now - lastErrorTrackAt >= 60_000) {
          lastErrorTrackAt = now
          track('sync', { success: false, duration_ms: now - startedAt })
        }
      } finally {
        running = false
      }
    }
    void sync()
    const timer = window.setInterval(sync, 8000)
    return () => {
      window.clearInterval(timer)
      if (hideTimer) clearTimeout(hideTimer)
      refreshRequest += 1
    }
  }, [bumpDataRevision, bumpGroupRevision, setCurrentGroupKey, setGroups, setSources, setSyncState, setView, resetSyncProgress])

  const pendingText =
    syncProgress.active && syncProgress.pending > 0
      ? `目录中还有 ${syncProgress.pending} 张图片未扫描，当前数据可能不完整`
      : null

  return (
    <div className="app">
      {syncStatus !== 'idle' && (
        <div className={`sync-bar sync-${syncStatus}`} role="status">
          <span>{syncStatus === 'syncing' ? '同步中' : syncStatus === 'success' ? '已同步' : '同步异常'}</span>
          <span className="sync-message">{syncMessage}</span>
        </div>
      )}
      {pendingText && (
        <div className="sync-warn" role="status">
          {pendingText}
        </div>
      )}
      {view === 'import' && <ImportPanel />}
      {view === 'groups' && <GroupList />}
      {view === 'swipe' && <SwipeDeck />}
      {view === 'arena' && <Arena />}
      {view === 'folder' && <FolderView />}
      {view === 'settings' && <Settings />}
    </div>
  )
}
