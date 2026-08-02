import { useEffect, useRef, useState } from 'react'
import { useStore } from '../store'
import {
  ARROW_ROW,
  ARROW_ROW2,
  KEYBOARD_ROWS,
  KEYMAP_ACTIONS,
  bindingDisplay,
  isModifierKey,
  keyCap,
  keyIdForEvent,
  keyIsSpace,
  keyIsWide,
  type Binding,
  type Keymap,
} from '../keymap'

const PALETTE = ['#6c8cff', '#81c784', '#ffb74d', '#e57373', '#ba68c8', '#4dd0e1', '#ff8a65', '#aed581', '#f06292', '#7986cb']

function actionColor(key: keyof Keymap): string {
  const i = KEYMAP_ACTIONS.findIndex((a) => a.key === key)
  return PALETTE[i % PALETTE.length]
}

function normalizeKeyId(k: string): string {
  return k === 'Space' ? ' ' : k
}

function triggerKey(binding: Binding | undefined): string {
  if (!binding) return ''
  const parts = binding.split('+').filter(Boolean)
  return normalizeKeyId(parts[parts.length - 1] ?? '')
}

/** Actions whose binding lands on a given physical key (as its trigger). */
function actionsOnKey(key: string, keymap: Keymap) {
  return KEYMAP_ACTIONS.filter((a) => triggerKey(keymap[a.key]) === key)
}

function isConflicting(actionKey: keyof Keymap, keymap: Keymap): boolean {
  const a = KEYMAP_ACTIONS.find((x) => x.key === actionKey)
  if (!a) return false
  const b = keymap[actionKey]
  // Only same-mode duplicates are real conflicts: swipe and arena are never
  // mounted together, so sharing arrow keys / Backspace across modes is fine.
  return KEYMAP_ACTIONS.some(
    (x) => x.key !== actionKey && x.group === a.group && keymap[x.key] === b && b !== '',
  )
}

const GROUPS: { id: 'swipe' | 'arena' | 'global'; label: string }[] = [
  { id: 'swipe', label: '滑卡模式' },
  { id: 'arena', label: '擂台模式' },
  { id: 'global', label: '通用' },
]

export function KeymapPanel() {
  const keybindings = useStore((s) => s.keybindings)
  const setKeybinding = useStore((s) => s.setKeybinding)
  const resetKeybindings = useStore((s) => s.resetKeybindings)
  // Action currently waiting for a new key press.
  const [capturing, setCapturing] = useState<keyof Keymap | null>(null)
  const heldModsRef = useRef<string[]>([])
  const [heldMods, setHeldMods] = useState<string[]>([])

  useEffect(() => {
    if (!capturing) return
    heldModsRef.current = []
    setHeldMods([])
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        setCapturing(null)
        return
      }
      e.preventDefault()
      e.stopPropagation()
      const keyId = keyIdForEvent(e)
      if (isModifierKey(keyId)) {
        if (!heldModsRef.current.includes(keyId)) {
          heldModsRef.current = [...heldModsRef.current, keyId]
          setHeldMods(heldModsRef.current)
        }
        return
      }
      setKeybinding(capturing, [...heldModsRef.current, keyId].join('+'))
      setCapturing(null)
    }
    const onKeyUp = (e: KeyboardEvent) => {
      const keyId = keyIdForEvent(e)
      if (!isModifierKey(keyId)) return
      const idx = heldModsRef.current.indexOf(keyId)
      if (idx === -1) return
      const next = [...heldModsRef.current.slice(0, idx), ...heldModsRef.current.slice(idx + 1)]
      heldModsRef.current = next
      setHeldMods(next)
      // The modifier was the only thing pressed → bind it as a bare modifier
      // (e.g. Shift as the arena arm-hide trigger).
      if (next.length === 0) {
        setKeybinding(capturing, keyId)
        setCapturing(null)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    window.addEventListener('keyup', onKeyUp)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      window.removeEventListener('keyup', onKeyUp)
    }
  }, [capturing, setKeybinding])

  function renderKey(key: string, rowIndex: string | number, colIndex: number) {
    const acts = actionsOnKey(key, keybindings)
    const usedAsModifier = KEYMAP_ACTIONS.some((a) => {
      const parts = keybindings[a.key].split('+').filter(Boolean)
      return parts.slice(0, -1).includes(key)
    })
    const conflicted = acts.some((a) => isConflicting(a.key, keybindings))
    const wide = keyIsWide(key)
    const space = keyIsSpace(key)
    return (
      <div
        key={`${rowIndex}-${colIndex}`}
        className={`kb-key ${wide ? 'kb-wide' : ''} ${space ? 'kb-space' : ''} ${conflicted ? 'kb-conflict' : ''}`}
        title={acts.length ? acts.map((a) => `${a.label}（${bindingDisplay(keybindings[a.key])}）`).join('\n') : undefined}
      >
        <span className="kb-cap">{keyCap(key)}</span>
        {usedAsModifier && <span className="kb-mod-dot" title="参与组合键" />}
        {acts.length > 0 && (
          <div className="kb-chips">
            {acts.map((a) => (
              <span key={a.key} className="kb-chip" style={{ color: actionColor(a.key) }}>
                {a.short}
              </span>
            ))}
          </div>
        )}
      </div>
    )
  }

  return (
    <div className="keymap-panel">
      <div className="keymap-head">
        <h3 style={{ margin: 0 }}>键位设置（可视化）</h3>
        <div className="row" style={{ margin: 0 }}>
          <button className="ghost" onClick={resetKeybindings} title="恢复所有按键为默认值">
            恢复默认
          </button>
        </div>
      </div>
      <p className="hint">
        点击下方操作行右侧「改绑」后按下新按键（可组合：先按 Shift/Ctrl/Alt/Win，再按主键；仅按修饰键则绑定修饰键本身，Esc 取消）。
        冲突项以红色边框标出。
      </p>

      <div className="keyboard-vis">
        {KEYBOARD_ROWS.map((row, ri) => (
          <div className="kb-row" key={ri}>
            {row.map((k, ci) => renderKey(k, ri, ci))}
          </div>
        ))}
        <div className="kb-row kb-row-center">
          {ARROW_ROW.map((k, ci) => renderKey(k, 'arrows', ci))}
        </div>
        <div className="kb-row kb-row-center">
          {ARROW_ROW2.map((k, ci) => renderKey(k, 'arrows2', ci))}
        </div>
      </div>

      {GROUPS.map((g) => (
        <div className="keymap-group" key={g.id}>
          <h4>{g.label}</h4>
          <table className="keymap-table">
            <tbody>
              {KEYMAP_ACTIONS.filter((a) => a.group === g.id).map((a) => {
                const binding = keybindings[a.key]
                const conflict = isConflicting(a.key, keybindings)
                const isCapturing = capturing === a.key
                return (
                  <tr key={a.key} className={conflict ? 'km-conflict' : ''}>
                    <td>
                      <span className="km-dot" style={{ background: actionColor(a.key) }} />
                      {a.label}
                    </td>
                    <td className="km-binding">
                      <code>{bindingDisplay(binding)}</code>
                      {conflict && <span className="km-warn">⚠ 与其他操作冲突</span>}
                    </td>
                    <td className="km-actions">
                      {isCapturing ? (
                        <span className="km-capturing">
                          {heldMods.length > 0 ? `已按：${heldMods.join('+')}，再按主键…` : '请按键（Esc 取消）'}
                        </span>
                      ) : (
                        <button className="ghost" onClick={() => setCapturing(a.key)}>
                          改绑
                        </button>
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      ))}
    </div>
  )
}
