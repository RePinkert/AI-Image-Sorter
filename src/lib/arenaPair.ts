import type { ImageRow } from '../types'

// Weight-band edges for score distance; far pairs never drop to zero so a
// decisive match is still reachable (skip key remains an escape hatch).
const CLOSE_BAND = 5
const MILD_BAND = 10
const FAR_BAND = 25
export const WEIGHT_CLOSE = 1.0
const WEIGHT_MILD = 0.5
const WEIGHT_FAR = 0.2
const WEIGHT_STRONG_FAR = 0.08

const WEIGHT_NOVELTY = 3.0
const WEIGHT_RECENT_PAIR = 0.15
const WEIGHT_STALE_MIN = 0.6
const WEIGHT_STALE_MAX = 1.6

export function pairKey(a: number, b: number) {
  return a < b ? `${a}:${b}` : `${b}:${a}`
}

function allPairs(images: ImageRow[]): [ImageRow, ImageRow][] {
  const result: [ImageRow, ImageRow][] = []
  for (let i = 0; i < images.length; i += 1) {
    for (let j = i + 1; j < images.length; j += 1) result.push([images[i], images[j]])
  }
  return result
}

function diffWeight(diff: number) {
  if (diff < CLOSE_BAND) return WEIGHT_CLOSE
  if (diff < MILD_BAND) return WEIGHT_MILD
  if (diff < FAR_BAND) return WEIGHT_FAR
  return WEIGHT_STRONG_FAR
}

function staleWeight(
  imageId: number,
  lastShownRound: Record<number, number> | null,
  round: number,
  count: number,
) {
  if (!lastShownRound) return 1
  const seen = lastShownRound[imageId]
  if (seen == null) return WEIGHT_STALE_MAX
  const age = (round - seen) / Math.max(1, count)
  const t = Math.max(0, Math.min(1, age))
  return WEIGHT_STALE_MIN + (WEIGHT_STALE_MAX - WEIGHT_STALE_MIN) * t
}

function weightedSample<T>(entries: [T, number][], rng: () => number, fallback: T[]): T | null {
  const total = entries.reduce((sum, [, w]) => sum + w, 0)
  if (total <= 0) {
    if (fallback.length === 0) return null
    return fallback[Math.floor(rng() * fallback.length)]
  }
  let acc = rng() * total
  let pick = entries[entries.length - 1][0]
  for (const [item, w] of entries) {
    acc -= w
    if (acc < 0) {
      pick = item
      break
    }
  }
  return pick
}

export interface PickArenaPairArgs {
  images: ImageRow[]
  scores: Record<number, number>
  recentPairs: Iterable<string>
  lastShownRound?: Record<number, number> | null
  round?: number
  rng?: () => number
}

export function pickArenaPair(args: PickArenaPairArgs): [ImageRow, ImageRow] | null {
  const { images, scores, recentPairs, lastShownRound = null, round = 0 } = args
  const rng = args.rng ?? Math.random
  const count = images.length
  if (count < 2) return null

  const seen = new Set<string>()
  for (const key of recentPairs) seen.add(key)

  const weighted: [[ImageRow, ImageRow], number][] = []
  for (const pair of allPairs(images)) {
    const [a, b] = pair
    const diff = Math.abs((scores[a.id] ?? 50) - (scores[b.id] ?? 50))
    let w = diffWeight(diff)
    if (scores[a.id] == null || scores[b.id] == null) w *= WEIGHT_NOVELTY
    if (seen.has(pairKey(a.id, b.id))) w *= WEIGHT_RECENT_PAIR
    w *= staleWeight(a.id, lastShownRound, round, count)
    w *= staleWeight(b.id, lastShownRound, round, count)
    weighted.push([pair, w])
  }

  const fallback = weighted.map(([pair]) => pair)
  const pick = weightedSample(weighted, rng, fallback)
  return pick ?? null
}
