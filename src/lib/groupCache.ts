import type { ImageRow } from '../types'

// 组相关的客户端缓存（列表里的缩略图、展开行的图片条）。
//
// 为什么单独成模块：这些缓存是**模块级**的 —— 组件卸载重挂后仍然命中旧值。
// 而组成员一变（搬运 / 合并 / 拆分 / 重聚类），旧值就不再代表这个组了。
// 漏掉一次作废 = 用户看到「已经搬走了，缩略图还是旧的」。
//
// 所以作废必须是**全局一次**（见 invalidateGroupCaches），而不是每个调用点
// 各自记得清 —— 那种写法必然会漏。

const thumbCache = new Map<string, string[]>()
const thumbInflight = new Map<string, Promise<string[]>>()
const rowImageCache = new Map<string, Promise<ImageRow[]>>()

export function getThumbCache(key: string): string[] | undefined {
  return thumbCache.get(key)
}

export function setThumbCache(key: string, paths: string[]): void {
  thumbCache.set(key, paths)
}

export function deleteThumbCache(key: string): void {
  thumbCache.delete(key)
}

export function getThumbInflight(key: string): Promise<string[]> | undefined {
  return thumbInflight.get(key)
}

export function setThumbInflight(key: string, request: Promise<string[]>): void {
  thumbInflight.set(key, request)
}

export function deleteThumbInflight(key: string): void {
  thumbInflight.delete(key)
}

export function getRowImageCache(key: string): Promise<ImageRow[]> | undefined {
  return rowImageCache.get(key)
}

export function setRowImageCache(key: string, request: Promise<ImageRow[]>): void {
  rowImageCache.set(key, request)
}

export function deleteRowImageCache(key: string): void {
  rowImageCache.delete(key)
}

/**
 * 组成员关系变了：所有组相关的客户端缓存一次性作废。
 *
 * 源组与目标组**都要**失效 —— 搬走一张图，源组的缩略图与目标组的缩略图
 * 都换了内容，只刷一边等于没刷。
 */
export function invalidateGroupCaches(): void {
  thumbCache.clear()
  thumbInflight.clear()
  rowImageCache.clear()
}
