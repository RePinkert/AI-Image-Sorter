import { useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'framer-motion'
import {
  assetUrl,
  confirmAction,
  errorMessage,
  getGroupThumbnails,
  hideGroupImages,
  listGroupImages,
  listGroups,
  listMoveTargets,
  suggestMoveTargets,
  mergeGroups,
  moveImagesToGroup,
  reclusterL1,
  splitImages,
  toggleHiddenAction,
  trashImage,
} from '../api'
import type {
  Granularity,
  GroupInfo,
  GroupThumbDto,
  ImageRow,
  ModelDeviationDimension,
  MoveSuggestion,
} from '../types'
import {
  MODEL_DEVIATION_DIMENSIONS,
  MODEL_DEVIATION_DIMENSION_LABELS,
} from '../types'
import { useStore } from '../store'
import { GRANULARITY_LEVELS, groupLabel } from '../lib/groupLabel'
import {
  deleteRowImageCache,
  deleteThumbCache,
  deleteThumbInflight,
  getRowImageCache,
  getThumbCache,
  getThumbInflight,
  setRowImageCache,
  setThumbCache,
  setThumbInflight,
} from '../lib/groupCache'
import {
  applyListAnchor,
  captureListAnchor,
  setFocusKey,
} from '../lib/listAnchor'


function LazyGroupThumb({
  groupKey,
  level,
  reloadToken = 0,
}: {
  groupKey: string
  level: Granularity
  /** 组成员变化的代数。变了就说明这一格的缩略图不再代表这个组。 */
  reloadToken?: number
}) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const [paths, setPaths] = useState<string[]>([])
  const [visible, setVisible] = useState(false)

  useEffect(() => {
    const el = hostRef.current
    if (!el) return
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setVisible(true)
          observer.disconnect()
        }
      },
      { rootMargin: '600px 0px' },
    )
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  // 组成员变了 → 这一格的缩略图不再代表这个组 → 丢掉缓存与在途结果，重取。
  // 声明在取数 effect **之前**：同一次提交里先作废再取数，才能保证这次拿到
  // 的是新成员，而不是作废前就已经在途的旧结果。
  //
  // 用 ref 记住「上次见到的代数」：组件刚挂载时不该作废缓存（那会让每次
  // 打开移动对话框都把全部目标组重取一遍）。全局作废由 invalidateGroupCaches
  // 负责，这里只处理「自己还挂着，但组变了」。
  const lastRevisionRef = useRef(reloadToken)
  useEffect(() => {
    if (reloadToken === lastRevisionRef.current) return
    lastRevisionRef.current = reloadToken
    const cacheKey = `${level}:${groupKey}`
    deleteThumbCache(cacheKey)
    deleteThumbInflight(cacheKey)
    setPaths([])
  }, [reloadToken, level, groupKey])

  useEffect(() => {
    if (!visible) return
    const cacheKey = `${level}:${groupKey}`
    const cached = getThumbCache(cacheKey)
    if (cached && cached.length > 0) {
      setPaths(cached)
      return
    }
    let request = getThumbInflight(cacheKey)
    if (!request) {
      request = getGroupThumbnails([groupKey], level).then((rows: GroupThumbDto[]) => rows[0]?.thumb_paths ?? [])
      setThumbInflight(cacheKey, request)
    }
    let cancelled = false
    void request.then((next) => {
      setThumbCache(cacheKey, next)
      deleteThumbInflight(cacheKey)
      if (!cancelled) setPaths(next)
    }).catch(() => {
      deleteThumbInflight(cacheKey)
    })
    return () => { cancelled = true }
  }, [groupKey, level, visible, reloadToken])

  return (
    <div className="group-thumb" ref={hostRef}>
      {paths.length > 0 ? (
        <div className={`group-thumb-grid count-${Math.min(paths.length, 4)}`}>
          {paths.map((path, index) => (
            <img key={`${path}-${index}`} src={assetUrl(path)} alt="" draggable={false} loading="lazy" decoding="async" />
          ))}
        </div>
      ) : <div className="thumb-placeholder" />}
    </div>
  )
}

function LazyRowImages({
  groupKey,
  level,
  selected,
  onToggleSelect,
  reloadToken,
}: {
  groupKey: string
  level: Granularity
  /** Selected image ids (scoped to this row). */
  selected: Set<number>
  onToggleSelect: (id: number) => void
  /** Bumped by the parent after a row mutation so cached rows refetch. */
  reloadToken: number
}) {
  const cacheKey = `${level}:${groupKey}`
  const hostRef = useRef<HTMLDivElement | null>(null)
  const [images, setImages] = useState<ImageRow[]>([])
  const [loaded, setLoaded] = useState(false)
  const [visible, setVisible] = useState(false)
  const groupRevision = useStore((s) => s.groupRevision)

  useEffect(() => {
    const el = hostRef.current
    if (!el) return
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setVisible(true)
          observer.disconnect()
        }
      },
      { rootMargin: '300px 0px' },
    )
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  useEffect(() => {
    if (!visible || loaded) return
    let request = getRowImageCache(cacheKey)
    if (!request) {
      request = listGroupImages(groupKey, level)
      setRowImageCache(cacheKey, request)
    }
    let cancelled = false
    void request
      .then((rows) => {
        if (!cancelled) {
          setImages(rows)
          setLoaded(true)
        }
      })
      .catch(() => deleteRowImageCache(cacheKey))
    return () => {
      cancelled = true
    }
  }, [cacheKey, groupKey, level, loaded, visible])

  // A fresh reloadToken invalidates the (now stale) cached promise.
  useEffect(() => {
    if (reloadToken === 0) return
    deleteRowImageCache(cacheKey)
    setLoaded(false)
    setImages([])
    setVisible(false)
    // Re-arm the observer immediately if the strip is already on screen.
    const el = hostRef.current
    if (el) setVisible(true)
  }, [reloadToken, cacheKey])

  // 组成员变了（别的组搬进来 / 搬走）→ 这一行的图片条也不再代表这个组。
  // 与 reloadToken 分开：前者是「本行被操作了」，后者是「全组的成员变了」。
  const lastRevisionRef = useRef(groupRevision)
  useEffect(() => {
    if (groupRevision === lastRevisionRef.current) return
    lastRevisionRef.current = groupRevision
    deleteRowImageCache(cacheKey)
    setLoaded(false)
    setImages([])
    setVisible(false)
    const el = hostRef.current
    if (el) setVisible(true)
  }, [groupRevision, cacheKey])

  if (!loaded) {
    return <div className="row-image-strip" ref={hostRef}><div className="thumb-placeholder" /></div>
  }
  return (
    <div className="row-image-strip">
      {images.map((img) => (
        <button
          key={img.id}
          type="button"
          className={`row-img-wrap ${selected.has(img.id) ? 'row-img-selected' : ''}`}
          title={`${img.filename}${img.hidden ? '（已屏蔽）' : ''} — 点击选择`}
          onClick={() => onToggleSelect(img.id)}
        >
          <img
            className={img.hidden ? 'row-img hidden-img' : 'row-img'}
            src={assetUrl(img.abs_path)}
            alt={img.filename}
            draggable={false}
            loading="lazy"
            decoding="async"
          />
          {selected.has(img.id) && <span className="row-img-check">✓</span>}
        </button>
      ))}
    </div>
  )
}

export function GroupList() {
  const setView = useStore((s) => s.setView)
  const groups = useStore((s) => s.groups)
  const setGroups = useStore((s) => s.setGroups)
  const setCurrentGroupKey = useStore((s) => s.setCurrentGroupKey)
  const currentSourceId = useStore((s) => s.currentSourceId)
  const setCurrentSourceId = useStore((s) => s.setCurrentSourceId)
  const setL2Threshold = useStore((s) => s.setL2Threshold)
  const granularity = useStore((s) => s.granularity)
  const setGranularity = useStore((s) => s.setGranularity)
  const sources = useStore((s) => s.sources)
  const dataRevision = useStore((s) => s.dataRevision)
  const groupRevision = useStore((s) => s.groupRevision)
  const bumpGroupRevision = useStore((s) => s.bumpGroupRevision)
  const [mergeMode, setMergeMode] = useState(false)
  const [selectedKeys, setSelectedKeys] = useState<Set<string>>(new Set())
  const [mergeBusy, setMergeBusy] = useState(false)
  const [mergeMsg, setMergeMsg] = useState<string | null>(null)
  /** Groups awaiting the confirm dialog before the merge actually runs. */
  const [confirmGroups, setConfirmGroups] = useState<GroupInfo[] | null>(null)
  const [loadingGroups, setLoadingGroups] = useState(groups.length === 0)
  const [groupError, setGroupError] = useState('')
  const groupRequestRef = useRef(0)
  /** The single wheel-selected Model偏差 dimension (defaults to base_model). */
  const [l1Dimension, setL1Dimension] = useState<ModelDeviationDimension>('base_model')
  /** Hoists the last computed dimension one rotation past the source's own so
   *  the badge always shows what the CURRENT groups represent. */
  const source = currentSourceId == null ? null : sources.find((s) => s.id === currentSourceId)
  const storedDim = source?.model_deviation_dimensions?.find(
    (d): d is ModelDeviationDimension =>
      (MODEL_DEVIATION_DIMENSIONS as string[]).includes(d),
  )
  useEffect(() => {
    if (source) setL1Dimension(storedDim ?? 'base_model')
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentSourceId, storedDim])
  const [reclusterBusy, setReclusterBusy] = useState(false)
  const reclusterTargetRef = useRef<ModelDeviationDimension | null>(null)
  // Native non-passive wheel listener: React's synthetic onWheel is passive,
  // so preventDefault could not stop the page from scrolling alongside. The
  // ref mirrors the live dimension (handlers below never go stale).
  const l1WheelWrapRef = useRef<HTMLDivElement | null>(null)
  const l1DimensionRef = useRef<ModelDeviationDimension>('base_model')
  const currentSourceIdRef = useRef<number | null>(currentSourceId)
  const reclusterBusyRef = useRef(false)
  useEffect(() => {
    currentSourceIdRef.current = currentSourceId
  }, [currentSourceId])
  useEffect(() => {
    l1DimensionRef.current = l1Dimension
  }, [l1Dimension])
  async function runRecluster() {
    if (currentSourceIdRef.current == null) return
    const busyTarget = reclusterTargetRef.current
    reclusterTargetRef.current = null
    if (busyTarget == null) return
    reclusterBusyRef.current = true
    setReclusterBusy(true)
    try {
      await reclusterL1(currentSourceIdRef.current, busyTarget)
      await loadGroups(currentSourceIdRef.current, useStore.getState().granularity)
      // 重聚类把组全换了一遍 → 缩略图全部作废。
      useStore.getState().bumpGroupRevision()
    } catch (error) {
      setGroupError(errorMessage(error))
    } finally {
      reclusterBusyRef.current = false
      setReclusterBusy(false)
      if (reclusterTargetRef.current != null) void runRecluster()
    }
  }
  useEffect(() => {
    const el = l1WheelWrapRef.current
    if (!el) return
    const handler = (event: WheelEvent) => {
      event.preventDefault()
      event.stopPropagation()
      if (currentSourceIdRef.current == null) return
      const idx = MODEL_DEVIATION_DIMENSIONS.indexOf(l1DimensionRef.current)
      const step = event.deltaY > 0 ? 1 : -1
      const next =
        MODEL_DEVIATION_DIMENSIONS[
          (idx + step + MODEL_DEVIATION_DIMENSIONS.length) % MODEL_DEVIATION_DIMENSIONS.length
        ]
      if (next === l1DimensionRef.current) return
      l1DimensionRef.current = next
      setL1Dimension(next)
      reclusterTargetRef.current = next
      if (reclusterBusyRef.current) return
      void runRecluster()
    }
    el.addEventListener('wheel', handler, { passive: false })
    return () => el.removeEventListener('wheel', handler)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  async function loadGroups(sourceId: number | null, level: Granularity) {
    const request = ++groupRequestRef.current
    setCurrentSourceId(sourceId)
    setLoadingGroups(true)
    setGroupError('')
    // The source's persisted threshold is authoritative — keep the Settings
    // slider / merge suggestions in sync with what the backend actually
    // re-clusters at.
    if (sourceId != null) {
      const src = sources.find((s) => s.id === sourceId)
      if (src?.l2_threshold != null) setL2Threshold(src.l2_threshold)
    }
    try {
      const g = await listGroups(sourceId ?? undefined, level)
      if (request !== groupRequestRef.current) return
      setGroups(g)
    } catch (error) {
      if (request !== groupRequestRef.current) return
      setGroupError(errorMessage(error))
    } finally {
      if (request === groupRequestRef.current) setLoadingGroups(false)
    }
  }

  useEffect(() => {
    if (groups.length === 0 && sources.length > 0) {
      void loadGroups(currentSourceId ?? sources[0].id, granularity)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sources])

  useEffect(() => {
    if (dataRevision > 0) void loadGroups(currentSourceId, granularity)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataRevision])

  useEffect(() => () => {
    groupRequestRef.current += 1
  }, [])


  function onLevelChange(level: Granularity) {
    setGranularity(level)
    if (level !== 2) exitMergeMode()
    void loadGroups(currentSourceId, level)
  }

  function exitMergeMode() {
    setMergeMode(false)
    setSelectedKeys(new Set())
    setMergeMsg(null)
    setConfirmGroups(null)
  }

  function toggleMergeSelect(key: string) {
    setSelectedKeys((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  async function reloadGroupsAtLevel() {
    if (currentSourceId == null) return
    await loadGroups(currentSourceId, granularity)
  }

  // ── 列表视口锚点 ───────────────────────────────────────────────────────
  // 离开分组列表时组件会卸载，而列表没有内部滚动容器（滚的是文档），
  // 滚动位置随之消失 → 回来时人在列表顶部。用户处理列表下半段的零碎
  // 分组时，每搬一个组就要重新滚回去一次，非常累。
  //
  // 口径：滚动时记下「视口顶部那一行」，回来时**按行**还原（不是按像素）
  // —— 行被删掉时下面整体上移，按行还原才能保证眼睛看到的内容不变。
  // 详见 src/lib/listAnchor.ts。
  const userScrollingRef = useRef(false)

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null
    const onScroll = () => {
      userScrollingRef.current = true
      if (timer) clearTimeout(timer)
      // 滚动停下来后记一次锚点。
      //
      // ⚠ 锚点**只在滚动时记**，不能在卸载时记 —— 卸载那一刻布局已经被
      // 下一个视图接管，文档高度塌缩会把滚动位置钳到很小；再对已消失的
      // 行调用 getBoundingClientRect() 还会强制重排，把「钳制后的滚动
      // 位置」当成锚点存下来（实测存成了 scrollTop=25，回来时直接落到
      // 列表顶部附近）。滚动时记就没有这个问题：那时列表还在。
      timer = setTimeout(() => {
        userScrollingRef.current = false
        captureListAnchor(currentSourceId, granularity)
      }, 100)
    }
    window.addEventListener('scroll', onScroll, { passive: true })
    return () => {
      window.removeEventListener('scroll', onScroll)
      if (timer) clearTimeout(timer)
    }
  }, [currentSourceId, granularity])

  // 分组加载完成后恢复位置。分组数组每次重载都是新引用 → 这个 effect
  // 会在每次刷新后跑一次，把视口放回原位（行被删掉导致下移时由它补偿）。
  useEffect(() => {
    if (loadingGroups) return
    applyListAnchor({
      sourceId: currentSourceId,
      granularity,
      userScrolling: userScrollingRef.current,
    })
  }, [groups, loadingGroups, currentSourceId, granularity])

  async function doMerge() {
    const keys = Array.from(selectedKeys)
    if (keys.length < 2 || mergeBusy) return
    // Never merge from the "所有源" view: group keys are shared across
    // sources, so an unscoped merge would sweep in images the user never
    // selected. Ask for confirmation with the selected groups' thumbnails.
    if (currentSourceId == null) {
      setMergeMsg('请先在顶部选择具体来源目录，再执行合并（避免误并其他来源的图片）')
      return
    }
    setConfirmGroups(groups.filter((g) => selectedKeys.has(g.group_key)))
  }

  async function doMergeConfirmed() {
    const keys = Array.from(selectedKeys)
    if (keys.length < 2 || mergeBusy || currentSourceId == null) return
    setConfirmGroups(null)
    setMergeBusy(true)
    setMergeMsg(null)
    try {
      const r = await mergeGroups(2, keys, currentSourceId)
      setMergeMsg(`已合并 ${r.moved} 张到同一分组（自动重聚类不会再拆开，可在文件夹视角撤销合并）`)
      setSelectedKeys(new Set())
      // 合并改变了成员 → 缩略图与图片条都要重取。
      bumpGroupRevision()
      await reloadGroupsAtLevel()
    } catch (e) {
      setMergeMsg(`合并失败：${errorMessage(e)}`)
    } finally {
      setMergeBusy(false)
    }
  }

  function openGroup(g: GroupInfo) {
    setCurrentGroupKey(g.group_key)
    // 回来时落在这一行上（用户刚看过它）。
    setFocusKey(g.group_key)
    setView('swipe')
  }

  function openFolder(g: GroupInfo) {
    setCurrentGroupKey(g.group_key)
    setFocusKey(g.group_key)
    setView('folder')
  }


  const syncProgress = useStore((s) => s.syncProgress)
  const syncing = syncProgress.active && syncProgress.pending > 0
  const l2ListView = useStore((s) => s.l2ListView)
  const setL2ListView = useStore((s) => s.setL2ListView)
  const listMode = granularity === 2 && l2ListView
  const [openRowKeys, setOpenRowKeys] = useState<Set<string>>(new Set())
  const [listFocusKey, setListFocusKey] = useState<string | null>(null)
  const [rowActionMsg, setRowActionMsg] = useState<string | null>(null)
  const [rowActionBusy, setRowActionBusy] = useState(false)
  /** Per-row selected image ids — when non-empty, the row's ⇆/⊗/🗑 buttons
   *  act on the SELECTION instead of the whole group. */
  const [rowSelections, setRowSelections] = useState<Record<string, number[]>>({})
  /** Bumped after each row mutation so the inline strip refetches. */
  const [rowImagesToken, setRowImagesToken] = useState<Record<string, number>>({})
  /** Pending migration request (whole-group or selected images). */
  const [moveRequest, setMoveRequest] = useState<{
    rowKey: string
    label: string
    imageIds: number[]
  } | null>(null)
  const [moveTargets, setMoveTargets] = useState<GroupInfo[] | null>(null)
  const [moveSuggestions, setMoveSuggestions] = useState<MoveSuggestion[]>([])
  const [moveBusy, setMoveBusy] = useState(false)

  function selectionOf(key: string): number[] {
    return rowSelections[key] ?? []
  }

  function toggleRowImageSelect(rowKey: string, id: number) {
    setRowSelections((prev) => {
      const cur = prev[rowKey] ?? []
      const next = cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id]
      return { ...prev, [rowKey]: next }
    })
  }

  function clearRowSelection(rowKey: string) {
    setRowSelections((prev) => {
      const next = { ...prev }
      delete next[rowKey]
      return next
    })
  }

  function bumpRowImages(rowKey: string) {
    setRowImagesToken((prev) => ({ ...prev, [rowKey]: (prev[rowKey] ?? 0) + 1 }))
  }

  function toggleRowOpen(key: string) {
    setListFocusKey(key)
    setOpenRowKeys((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  /** Member ids for the current, matching the SAME source scoping the group
   *  list used. list_group_images is global (keys are shared across
   *  sources); filter by source for safety. */
  async function memberIdsOf(rowKey: string): Promise<number[]> {
    const rows = await listGroupImages(rowKey, granularity)
    return rows
      .filter((r) => currentSourceId == null || r.source_id === currentSourceId)
      .map((r) => r.id)
  }

  async function openMoveDialog(rowKey: string) {
    if (currentSourceId == null) {
      setRowActionMsg('请先在顶部选择具体来源目录，再执行迁移')
      return
    }
    setRowActionBusy(true)
    setRowActionMsg(null)
    try {
      const group = groups.find((g) => g.group_key === rowKey)
      const ids = selectionOf(rowKey).length > 0
        ? selectionOf(rowKey)
        : await memberIdsOf(rowKey)
      if (ids.length === 0) {
        setRowActionMsg('该组没有可迁移的成员')
        return
      }
      const targets = await listMoveTargets(currentSourceId, rowKey)
      setMoveRequest({
        rowKey,
        label: group ? groupLabel(group, granularity) : rowKey,
        imageIds: ids,
      })
      setMoveTargets(targets)
      setMoveSuggestions([])
      // 「可能相关」：推荐失败不该挡住主流程。
      void suggestMoveTargets(currentSourceId, ids, rowKey)
        .then((list) => setMoveSuggestions(list.slice(0, 12)))
        .catch(() => undefined)
    } catch (e) {
      setRowActionMsg(`加载迁移目标失败：${errorMessage(e)}`)
    } finally {
      setRowActionBusy(false)
    }
  }

  function closeMoveDialog() {
    setMoveRequest(null)
    setMoveTargets(null)
    setMoveSuggestions([])
  }

  async function moveConfirmed(targetKey: string | null) {
    if (!moveRequest || moveBusy) return
    setMoveBusy(true)
    try {
      const r = targetKey == null
        ? // "新建分组": split out into its own independent group (manual
          // pin, so re-clustering keeps it apart).
          await splitImages(2, moveRequest.imageIds)
        : await moveImagesToGroup(moveRequest.imageIds, targetKey)
      const targetLabel = targetKey == null ? '新分组' : '目标组'
      setRowActionMsg(`已迁移 ${r.moved} 张 → ${targetLabel}`)
      clearRowSelection(moveRequest.rowKey)
      bumpRowImages(moveRequest.rowKey)
      // 源组与目标组的成员都变了 → 两边的缩略图都要重取。
      bumpGroupRevision()
      // 整组搬走 → 回到列表时优先定位到它原来所在的那一行。
      setFocusKey(moveRequest.rowKey)
      closeMoveDialog()
      await reloadGroupsAtLevel()
    } catch (e) {
      setRowActionMsg(`迁移失败：${errorMessage(e)}`)
    } finally {
      setMoveBusy(false)
    }
  }

  async function hideRowScope(g: GroupInfo) {
    const ids = selectionOf(g.group_key)
    if (currentSourceId == null) {
      setRowActionMsg('请先在顶部选择具体来源目录，再执行屏蔽')
      return
    }
    setRowActionBusy(true)
    setRowActionMsg(null)
    try {
      if (ids.length > 0) {
        for (const id of ids) {
          await toggleHiddenAction(id, true)
        }
        setRowActionMsg(`已隐藏所选 ${ids.length} 张 — 可在文件夹视角撤销`)
        clearRowSelection(g.group_key)
        bumpRowImages(g.group_key)
        // 缩略图只取未屏蔽的图 → 屏蔽之后缩略图就变了。
        bumpGroupRevision()
      } else {
        const hiddenCount = await hideGroupImages(currentSourceId, g.group_key, true)
        setRowActionMsg(`已整组隐藏 ${hiddenCount} 张 — 可在文件夹视角撤销`)
        bumpGroupRevision()
      }
      await reloadGroupsAtLevel()
    } catch (e) {
      setRowActionMsg(`屏蔽失败：${errorMessage(e)}`)
    } finally {
      setRowActionBusy(false)
    }
  }

  async function deleteRowScope(g: GroupInfo) {
    const ids = selectionOf(g.group_key)
    const currentSource = currentSourceId
    if (currentSource == null) {
      setRowActionMsg('请先在顶部选择具体来源目录，再执行删除')
      return
    }
    setRowActionBusy(true)
    setRowActionMsg(null)
    try {
      const targetLabel = ids.length > 0
        ? `所选 ${ids.length} 张`
        : `${dangerGuardLabel(g)} 全部 ${g.count} 张`
      const ok = await confirmAction(
        `删除 ${targetLabel}图片？`,
        '所选图片将被移入系统回收站并从库中移除；文件可从桌面回收站恢复。',
      )
      if (!ok) return
      const doomed = ids.length > 0 ? ids : await memberIdsOf(g.group_key)
      let deleted = 0
      for (const id of doomed) {
        try {
          await trashImage(id)
          deleted += 1
        } catch {
          // One failure doesn't abort the rest of the batch.
        }
      }
      setRowActionMsg(`已删除 ${deleted} 张（回收站可恢复文件）`)
      clearRowSelection(g.group_key)
      bumpRowImages(g.group_key)
      // 成员没了 → 缩略图与图片条都要重取。
      bumpGroupRevision()
      await reloadGroupsAtLevel()
    } catch (e) {
      setRowActionMsg(`删除失败：${errorMessage(e)}`)
    } finally {
      setRowActionBusy(false)
    }
  }

  function dangerGuardLabel(g: GroupInfo): string {
    return `「${groupLabel(g, granularity)}」`
  }

  return (
    <div className="panel">
      <h2>分组列表</h2>
      <div className="row">
        <select
          value={currentSourceId ?? ''}
          onChange={(e) => {
            if (currentSourceId !== (e.target.value ? Number(e.target.value) : null)) {
              exitMergeMode()
            }
            void loadGroups(e.target.value ? Number(e.target.value) : null, granularity)
          }}
        >
          <option value="">所有源</option>
          {sources.map((s) => (
            <option key={s.id} value={s.id}>
              {s.alias ?? s.path}
            </option>
          ))}
        </select>
        <button type="button" className="btn-secondary" onClick={() => setView('import')}>+ 导入新源</button>
        <button type="button" className="btn-secondary" onClick={() => setView('settings')}>标签设置 / 导出</button>
        {granularity === 2 && (
          <button
            type="button"
            className="btn-secondary"
            onClick={() => setL2ListView(!l2ListView)}
            title="Prompt偏差 层级的呈现方式：卡片网格（旧视图，作为回滚保底）或可展开的分组列表"
          >
            {l2ListView ? '切换到卡片视图' : '切换到列表视图'}
          </button>
        )}
        {granularity === 2 && (
          <button
            type="button"
            className={mergeMode ? 'gran-active' : 'btn-secondary'}
            disabled={mergeBusy || currentSourceId == null}
            onClick={() => (mergeMode ? exitMergeMode() : setMergeMode(true))}
            title={currentSourceId == null
              ? '请先选择具体来源目录再合并（避免误并其他来源的图片）'
              : '勾选两个及以上 Prompt偏差 组，合并为同一组（自动重聚类不会拆开，可在文件夹视角撤销）'}
          >
            {mergeMode ? '合并分组…' : '合并分组'}
          </button>
        )}
      </div>

      <div className="granularity-bar">
        {GRANULARITY_LEVELS.map((l) =>
          l.value !== 1 ? (
            <button
              type="button"
              key={l.value}
              className={granularity === l.value ? 'gran-active' : ''}
              onClick={() => onLevelChange(l.value)}
            >
              {l.label}
            </button>
          ) : (
            <div
              key={l.value}
              ref={l1WheelWrapRef}
              className="l1-wheel-wrap"
              title={`滚轮切换模型参数维度：当前 ${MODEL_DEVIATION_DIMENSION_LABELS[l1Dimension]}（base_model → Text Encoder → VAE → LoRA → Embedding → ControlNet 循环）`}
            >
              <button
                type="button"
                className={granularity === 1 ? 'gran-active' : ''}
                onClick={() => onLevelChange(1)}
              >
                {l.label}
                <span className={`l1-dim-badge ${reclusterBusy ? 'busy' : ''}`}>
                  {reclusterBusy ? '聚类中…' : MODEL_DEVIATION_DIMENSION_LABELS[l1Dimension]}
                </span>
              </button>
            </div>
          ),
        )}
      </div>

      {loadingGroups && <p className="muted" role="status">正在加载分组…</p>}
      {groupError && (
        <div className="action-error" role="alert">
          <span>分组加载失败：{groupError}</span>
          <button type="button" onClick={() => void loadGroups(currentSourceId, granularity)}>重试</button>
        </div>
      )}
      {!loadingGroups && !groupError && groups.length === 0 && (
        <div className="empty-groups">
          <p className="empty-groups-title">暂无分组</p>
          <p className="muted">选择来源目录并扫描后，图片会按 prompt 自动聚类成组。</p>
          <button type="button" className="btn-secondary" onClick={() => setView('import')}>
            去导入来源目录
          </button>
        </div>
      )}
      {syncing && (
        <p className="sync-warn">扫描进行中，分组数据可能不完整。</p>
      )}
      {mergeMode && granularity === 2 && (
        <div className="merge-bar">
          <span>已选 {selectedKeys.size} 组（至少选 2 组）</span>
          <button type="button" disabled={mergeBusy || selectedKeys.size < 2} onClick={() => void doMerge()}>
            合并所选
          </button>
          <button type="button" className="ghost" disabled={mergeBusy} onClick={exitMergeMode}>
            退出
          </button>
          {mergeMsg && <span className="merge-msg">{mergeMsg}</span>}
        </div>
      )}
      {!listMode && (
        <div className="group-grid">
          {groups.map((g) => (
            <div
              key={g.group_key}
              data-group-key={g.group_key}
              className={`group-card ${mergeMode && granularity === 2 && selectedKeys.has(g.group_key) ? 'merge-selected' : ''}`}
              onClick={() => (mergeMode && granularity === 2 ? toggleMergeSelect(g.group_key) : openGroup(g))}
              role="button"
              tabIndex={0}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault()
                  mergeMode && granularity === 2 ? toggleMergeSelect(g.group_key) : openGroup(g)
                }
              }}
              title={groupLabel(g, granularity)}
            >
              {mergeMode && granularity === 2 && (
                <input
                  type="checkbox"
                  className="merge-check"
                  checked={selectedKeys.has(g.group_key)}
                  onChange={(e) => {
                    e.stopPropagation()
                    toggleMergeSelect(g.group_key)
                  }}
                  onClick={(e) => e.stopPropagation()}
                />
              )}
              <LazyGroupThumb groupKey={g.group_key} level={granularity} reloadToken={groupRevision} />
              <span className="group-count-badge">{g.count}</span>
              <div className="group-prompt">{groupLabel(g, granularity)}</div>
              {granularity === 2 && g.manually_merged && (
                <span className="merged-badge" title="该组为手动合并结果，自动重聚类不会拆开">
                  已手动合并
                </span>
              )}
              <div className="group-meta">
                {/* source_kind 全库恒为 'comfy'，单独展示是冗余（规则05），删去。 */}
                <span
                  className="group-meta-text"
                  title={granularity === 1 && currentSourceId != null
                    ? `${MODEL_DEVIATION_DIMENSION_LABELS[l1Dimension]} · ${g.checkpoint || '—'}`
                    : (g.checkpoint || '—')}
                >
                  {granularity === 1 && currentSourceId != null
                    ? `${MODEL_DEVIATION_DIMENSION_LABELS[l1Dimension]} · ${g.checkpoint || '—'}`
                    : (g.checkpoint || '—')}
                </span>
                <button
                  type="button"
                  className="folder-link"
                  onClick={(e) => {
                    e.stopPropagation()
                    openFolder(g)
                  }}
                  title="文件夹视角（速览评分 / 屏蔽 / 删除）"
                  style={{ marginLeft: 8 }}
                >
                  文件夹视角
                </button>
              </div>
              {granularity === 1 && g.model_facets && g.model_facets.length > 0 && (
                <div className="model-facets">
                  {g.model_facets.map((f) => (
                    <span key={f.model} className="model-chip" title={f.model}>
                      {f.model} <b>{f.count}</b>
                    </span>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
      {listMode && (
        <div className="group-list-rows" role="list">
          {rowActionMsg && <p className="sync-warn" role="status">{rowActionMsg}</p>}
          {groups.map((g) => {
            const open = openRowKeys.has(g.group_key)
            const focused = listFocusKey === g.group_key
            const selN = selectionOf(g.group_key).length
            const rowSel = openRowKeys.has(g.group_key) ? selectionOf(g.group_key) : []
            return (
              <div
                key={g.group_key}
                role="listitem"
                data-group-key={g.group_key}
                className={`group-row ${focused ? 'row-focused' : ''} ${open ? 'row-open' : ''} ${mergeMode && selectedKeys.has(g.group_key) ? 'merge-selected' : ''}`}
              >
                <div className="group-row-head">
                  {mergeMode ? (
                    <input
                      type="checkbox"
                      className="merge-check"
                      checked={selectedKeys.has(g.group_key)}
                      onChange={(e) => {
                        e.stopPropagation()
                        toggleMergeSelect(g.group_key)
                      }}
                      onClick={(e) => e.stopPropagation()}
                    />
                  ) : (
                    <button
                      type="button"
                      className={`row-expander ${open ? 'open' : ''}`}
                      title={open ? '折叠' : '展开显示该组图片'}
                      onClick={() => toggleRowOpen(g.group_key)}
                    >
                      ▸
                    </button>
                  )}
                  <div
                    className="group-row-title"
                    role="button"
                    tabIndex={0}
                    onClick={() => toggleRowOpen(g.group_key)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault()
                        toggleRowOpen(g.group_key)
                      }
                    }}
                  >
                    <span className="group-chevron">{open ? '▾' : '▸'}</span>
                    <span className="group-count-badge">{g.count}</span>
                    <span className="group-prompt">{groupLabel(g, granularity)}</span>
                    {g.manually_merged && <span className="merged-badge" title="该组为手动合并结果，自动重聚类不会拆开">已手动合并</span>}
                  </div>
                  <div className="group-row-actions">
                    <span className={`row-selection-badge ${selN > 0 ? 'has-sel' : ''}`}>
                      {selN > 0 ? `已选 ${selN} 张 · 按钮作用于所选` : '未选图片 · 按钮作用于整组'}
                      {selN > 0 && (
                        <button
                          type="button"
                          className="row-action row-clear"
                          disabled={rowActionBusy}
                          title="清除图片选择（回到整组操作）"
                          onClick={() => clearRowSelection(g.group_key)}
                        >
                          ✕
                        </button>
                      )}
                    </span>
                    <button
                      type="button"
                      className="row-action"
                      disabled={rowActionBusy}
                      title={selN > 0 ? `迁移所选 ${selN} 张到其他组或新建分组` : '整组迁移到其他组或新建分组'}
                      onClick={() => void openMoveDialog(g.group_key)}
                    >
                      ⇆
                    </button>
                    <button
                      type="button"
                      className="row-action"
                      disabled={rowActionBusy || currentSourceId == null}
                      title={selN > 0 ? `屏蔽所选 ${selN} 张（可在文件夹视角撤销）` : '整组隐藏（可在文件夹视角撤销）'}
                      onClick={() => void hideRowScope(g)}
                    >
                      ⊗
                    </button>
                    <button
                      type="button"
                      className="row-action row-action-danger"
                      disabled={rowActionBusy || currentSourceId == null}
                      title={selN > 0 ? `删除所选 ${selN} 张：移入系统回收站（Shift+Delete 语义）` : '整组删除：全体成员移入系统回收站（Shift+Delete 语义）'}
                      onClick={() => void deleteRowScope(g)}
                    >
                      <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true" focusable="false" className="icon-glyph">
                        <path
                          d="M1.5 3h9 M4.5 3V1.8h3V3 M2.8 3.2l.5 7.3h5.4l.5-7.3 M5 5v4 M7 5v4"
                          fill="none"
                          stroke="currentColor"
                          strokeWidth="1.1"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                        />
                      </svg>
                    </button>
                  </div>
                </div>
                {open && !mergeMode && (
                  <div className="group-row-body">
                    <LazyRowImages
                      groupKey={g.group_key}
                      level={granularity}
                      selected={new Set(rowSel)}
                      onToggleSelect={(id) => toggleRowImageSelect(g.group_key, id)}
                      reloadToken={rowImagesToken[g.group_key] ?? 0}
                    />
                    <div className="row-open-hint">
                      {currentSourceId == null
                        ? '请先在顶部选择具体来源目录，再使用组按钮操作'
                        : '点击图片多选；选中后 ⇆ ⊗ 删除按钮作用于所选图片，否则作用于整组'}
                    </div>
                  </div>
                )}
              </div>
            )
          })}
        </div>
      )}
      <AnimatePresence>
        {moveRequest && (
          <motion.div
            className="merge-confirm-overlay"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={closeMoveDialog}
          >
            <motion.div
              className="merge-confirm"
              role="dialog"
              aria-modal="true"
              aria-label="迁移图片"
              initial={{ opacity: 0, scale: 0.92, y: 10 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={{ opacity: 0, scale: 0.94, y: 6 }}
              transition={{ type: 'spring', stiffness: 320, damping: 28 }}
              onClick={(e) => e.stopPropagation()}
            >
              <h3>迁移 {moveRequest.imageIds.length} 张图片</h3>
              <p className="muted">来源：{moveRequest.label || '(无标签组)'}。迁移后自动重聚类不会拆开；目标可以是现有组，或新建一个分组。</p>
              <div className="move-columns">
                <section className="move-column">
                  <h4 className="move-column-title">
                    全部组 <span className="muted">{(moveTargets ?? []).length}</span>
                  </h4>
                  <div className="move-target-picker">
                    <button
                      type="button"
                      className="move-target new-group"
                      disabled={moveBusy}
                      onClick={() => void moveConfirmed(null)}
                      title="为所选图片新建一个独立分组（移出成新组）"
                    >
                      <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true" focusable="false" className="icon-glyph">
                        <path
                          d="M6 1.5v9 M1.5 6h9"
                          fill="none"
                          stroke="currentColor"
                          strokeWidth="1.1"
                          strokeLinecap="round"
                        />
                      </svg>
                      新建分组
                    </button>
                    {(moveTargets ?? []).map((t) => (
                      <button
                        type="button"
                        key={t.group_key}
                        className="move-target"
                        disabled={moveBusy}
                        title={t.prompt_pos.slice(0, 200)}
                        onClick={() => void moveConfirmed(t.group_key)}
                      >
                        <LazyGroupThumb groupKey={t.group_key} level={granularity} reloadToken={groupRevision} />
                        <span className="group-count-badge">{t.count}</span>
                        <span className="group-prompt">{t.prompt_pos.slice(0, 120) || '(无 prompt)'}</span>
                      </button>
                    ))}
                    {moveTargets != null && moveTargets.length === 0 && (
                      <p className="muted">同来源下暂无其他组 — 可选择新建分组。</p>
                    )}
                  </div>
                </section>
                <section className="move-column">
                  <h4 className="move-column-title">
                    可能相关 <span className="muted">{moveSuggestions.length}</span>
                  </h4>
                  {moveSuggestions.length === 0 ? (
                    <p className="muted">没有可推荐的目标组（需要先识图，或选中图与其它组差异过大）。</p>
                  ) : (
                    <div className="move-target-picker">
                      {moveSuggestions.map((s) => (
                        <button
                          type="button"
                          key={s.group_key}
                          className="move-target"
                          disabled={moveBusy}
                          title={`${s.basis === 'vision' ? '视觉相似' : '文本相似'} ${(s.score * 100).toFixed(0)}%`}
                          onClick={() => void moveConfirmed(s.group_key)}
                        >
                          <LazyGroupThumb groupKey={s.group_key} level={granularity} reloadToken={groupRevision} />
                          <span className="group-count-badge">{s.count}</span>
                          <span className="group-prompt">{s.prompt_pos.slice(0, 120) || '(无 prompt)'}</span>
                          <span className="move-score" data-basis={s.basis}>
                            {(s.score * 100).toFixed(0)}%
                          </span>
                        </button>
                      ))}
                    </div>
                  )}
                </section>
              </div>
              <div className="row">
                <button type="button" className="ghost" disabled={moveBusy} onClick={closeMoveDialog}>取消</button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
      <AnimatePresence>
        {confirmGroups && confirmGroups.length > 0 && (
          <motion.div
            className="merge-confirm-overlay"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={() => setConfirmGroups(null)}
          >
            <motion.div
              className="merge-confirm"
              role="dialog"
              aria-modal="true"
              aria-label="确认合并分组"
              initial={{ opacity: 0, scale: 0.92, y: 10 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={{ opacity: 0, scale: 0.94, y: 6 }}
              transition={{ type: 'spring', stiffness: 320, damping: 28 }}
              onClick={(e) => e.stopPropagation()}
            >
              <h3>确认合并这 {confirmGroups.length} 个组？</h3>
              <p className="muted">合并后 {confirmGroups.reduce((n, g) => n + g.count, 0)} 张图片进入同一分组，自动重聚类不会再拆开；合并后可在该组的文件夹视角撤销。</p>
              <div className="merge-confirm-groups">
                {confirmGroups.map((g, i) => (
                  <motion.div
                    key={g.group_key}
                    className="merge-confirm-group"
                    initial={{ opacity: 0, y: 14 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ delay: 0.05 * i, duration: 0.18 }}
                  >
<LazyGroupThumb groupKey={g.group_key} level={granularity} reloadToken={groupRevision} />
                    <span className="group-count-badge">{g.count}</span>
                    <div className="group-prompt">{groupLabel(g, granularity)}</div>
                  </motion.div>
                ))}
              </div>
              <div className="row">
                <button type="button" className="ghost" disabled={mergeBusy} onClick={() => setConfirmGroups(null)}>取消</button>
                <button type="button" disabled={mergeBusy} onClick={() => void doMergeConfirmed()}>确认合并</button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  )
}
