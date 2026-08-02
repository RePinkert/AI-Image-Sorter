// Central keyboard binding config. A Binding is a string like 'ArrowLeft',
// 'h', 'Space' or a combo 'Shift+ArrowLeft'. Combo parts are joined with '+';
// the first parts are required modifiers, the last part is the trigger key.

export type Binding = string

export interface Keymap {
  swipeLeft: Binding
  swipeRight: Binding
  swipeUp: Binding
  swipeDown: Binding
  swipeHide: Binding
  swipeRewind: Binding
  arenaVoteLeft: Binding
  arenaVoteRight: Binding
  arenaSkip: Binding
  arenaArmHide: Binding
  arenaHideLeft: Binding
  arenaHideRight: Binding
  arenaUndoHide: Binding
  closeLightbox: Binding
}

export const DEFAULT_KEYMAP: Keymap = {
  swipeLeft: 'ArrowLeft',
  swipeRight: 'ArrowRight',
  swipeUp: 'ArrowUp',
  swipeDown: 'ArrowDown',
  swipeHide: 'h',
  swipeRewind: 'Backspace',
  arenaVoteLeft: 'ArrowLeft',
  arenaVoteRight: 'ArrowRight',
  arenaSkip: 'Space',
  // Shift is a combo trigger: hold Shift to arm the hide hint, Shift+←/→ to
  // hide the matching card without releasing Shift.
  arenaArmHide: 'Shift',
  arenaHideLeft: 'Shift+ArrowLeft',
  arenaHideRight: 'Shift+ArrowRight',
  arenaUndoHide: 'Backspace',
  closeLightbox: 'Escape',
}

export interface KeymapAction {
  key: keyof Keymap
  label: string
  /** Short name shown on the visual keyboard key chip. */
  short: string
  group: 'swipe' | 'arena' | 'global'
}

export const KEYMAP_ACTIONS: KeymapAction[] = [
  { key: 'swipeLeft', label: '滑卡 · 差', short: '差', group: 'swipe' },
  { key: 'swipeRight', label: '滑卡 · 优', short: '优', group: 'swipe' },
  { key: 'swipeUp', label: '滑卡 · 待优化', short: '待优化', group: 'swipe' },
  { key: 'swipeDown', label: '滑卡 · 跳过', short: '跳过', group: 'swipe' },
  { key: 'swipeHide', label: '滑卡 · 屏蔽', short: '屏蔽', group: 'swipe' },
  { key: 'swipeRewind', label: '滑卡 · 回退', short: '回退', group: 'swipe' },
  { key: 'arenaVoteLeft', label: '擂台 · 左胜', short: '左胜', group: 'arena' },
  { key: 'arenaVoteRight', label: '擂台 · 右胜', short: '右胜', group: 'arena' },
  { key: 'arenaSkip', label: '擂台 · 跳过', short: '跳过', group: 'arena' },
  { key: 'arenaArmHide', label: '擂台 · 屏蔽提示（按住）', short: '屏蔽提示', group: 'arena' },
  { key: 'arenaHideLeft', label: '擂台 · 屏蔽左卡', short: '屏蔽左', group: 'arena' },
  { key: 'arenaHideRight', label: '擂台 · 屏蔽右卡', short: '屏蔽右', group: 'arena' },
  { key: 'arenaUndoHide', label: '擂台 · 撤销屏蔽', short: '撤销屏蔽', group: 'arena' },
  { key: 'closeLightbox', label: '通用 · 关闭灯箱', short: '关灯箱', group: 'global' },
]

const MODIFIERS = ['Shift', 'Control', 'Ctrl', 'Alt', 'Meta'] as const

const MODIFIER_STATE: Record<string, (e: KeyboardEvent) => boolean> = {
  Shift: (e) => e.shiftKey,
  Control: (e) => e.ctrlKey,
  Ctrl: (e) => e.ctrlKey,
  Alt: (e) => e.altKey,
  Meta: (e) => e.metaKey,
}

export function isModifierKey(key: string): boolean {
  return (MODIFIERS as readonly string[]).includes(key)
}

function keyMatches(pressed: string, expected: string): boolean {
  if (expected === 'Space') return pressed === ' ' || pressed === 'Spacebar'
  if (pressed.length === 1 && expected.length === 1) {
    return pressed.toLowerCase() === expected.toLowerCase()
  }
  return pressed === expected
}

/** True when `e` matches `binding`. Single-key bindings require no modifier
 *  to be held (so Shift+← never fires a plain ← binding); combos require the
 *  listed modifiers exactly. A bare modifier binding (e.g. 'Shift') fires on
 *  the fresh keydown of that modifier, not on auto-repeat. */
export function matchesBinding(binding: Binding | undefined, e: KeyboardEvent): boolean {
  if (!binding) return false
  const parts = binding.split('+').filter(Boolean)
  if (parts.length === 0) return false
  const trigger = parts[parts.length - 1]
  const mods = parts.slice(0, -1)

  // Modifier used as the trigger itself (e.g. hold Shift to arm hide).
  if (mods.length === 0 && isModifierKey(trigger)) {
    if (e.key !== trigger && !(trigger === 'Ctrl' && e.key === 'Control')) return false
    if (e.repeat) return false
    return true
  }

  // Exact modifier state match.
  for (const m of MODIFIERS) {
    const required = mods.includes(m)
    if (MODIFIER_STATE[m](e) !== required) return false
  }
  return keyMatches(e.key, trigger)
}

export function keyIdForEvent(e: KeyboardEvent): string {
  if (e.key === ' ') return 'Space'
  if (e.key === 'Escape') return 'Escape'
  if (e.key.length === 1) return e.key.toLowerCase()
  return e.key
}

/** Format a binding for display, e.g. 'Shift+ArrowLeft' → 'Shift + ←'. */
export function bindingDisplay(binding: Binding | undefined): string {
  if (!binding) return '—'
  const names: Record<string, string> = {
    ' ': 'Space',
    Space: 'Space',
    Spacebar: 'Space',
    ArrowLeft: '←',
    ArrowRight: '→',
    ArrowUp: '↑',
    ArrowDown: '↓',
    Backspace: 'Backspace',
    Escape: 'Esc',
    Shift: 'Shift',
    Control: 'Ctrl',
    Alt: 'Alt',
    Meta: 'Win',
    CapsLock: 'Caps',
    Enter: 'Enter',
    Tab: 'Tab',
  }
  return binding
    .split('+')
    .map((p) => names[p] ?? (p.length === 1 ? p.toUpperCase() : p))
    .join(' + ')
}

/** Physical keys involved in a binding (layout vocabulary): the trigger key
 *  plus any modifier keys, for the visual keyboard highlights. */
export function bindingTargetKeys(binding: Binding | undefined): string[] {
  if (!binding) return []
  const parts = binding.split('+').filter(Boolean)
  return parts
}

export const MODIFIER_LABEL: Record<string, string> = {
  Shift: 'Shift',
  Control: 'Ctrl',
  Ctrl: 'Ctrl',
  Alt: 'Alt',
  Meta: 'Win',
}

// Full US-style keyboard layout for the TraceBoard-like visualization.
// Each row is a list of key ids; the visual keyboard keys use these ids.
export const KEYBOARD_ROWS: string[][] = [
  ['Escape', 'F1', 'F2', 'F3', 'F4', 'F5', 'F6', 'F7', 'F8', 'F9', 'F10', 'F11', 'F12'],
  ['`', '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', '-', '=', 'Backspace'],
  ['Tab', 'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', '[', ']', '\\'],
  ['CapsLock', 'a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', ';', "'", 'Enter'],
  ['Shift', 'z', 'x', 'c', 'v', 'b', 'n', 'm', ',', '.', '/', 'Shift'],
  ['Control', 'Meta', 'Alt', ' ', 'Alt', 'Meta', 'Control'],
]

export const ARROW_ROW: string[] = ['ArrowUp']
export const ARROW_ROW2: string[] = ['ArrowLeft', 'ArrowDown', 'ArrowRight']

/** Physical key labels (layout vocabulary → display text). */
export function keyCap(key: string): string {
  const caps: Record<string, string> = {
    '`': '`',
    ' ': '',
    Space: '',
    ArrowUp: '↑',
    ArrowDown: '↓',
    ArrowLeft: '←',
    ArrowRight: '→',
    Control: 'Ctrl',
    Meta: 'Win',
    CapsLock: 'Caps',
    Escape: 'Esc',
  }
  if (key in caps) return caps[key]
  return key.length === 1 ? key.toUpperCase() : key
}

export function keyIsWide(key: string): boolean {
  return ['Backspace', 'Tab', 'CapsLock', 'Enter', 'Shift', 'Control', 'Meta', 'Alt', ' '].includes(key)
}

export function keyIsSpace(key: string): boolean {
  return key === ' ' || key === 'Space'
}
