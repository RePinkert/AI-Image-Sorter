import { useEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'

interface PopoverProps {
  trigger: ReactNode
  children: ReactNode
  open?: boolean
  onOpenChange?: (open: boolean) => void
}

export function Popover({ trigger, children, open: controlledOpen, onOpenChange }: PopoverProps) {
  const [internalOpen, setInternalOpen] = useState(false)
  const isControlled = controlledOpen !== undefined
  const open = isControlled ? controlledOpen : internalOpen

  const setOpen = (v: boolean) => {
    if (isControlled) {
      onOpenChange?.(v)
    } else {
      setInternalOpen(v)
    }
  }

  const triggerRef = useRef<HTMLDivElement>(null)
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    function onDown(e: MouseEvent) {
      const el = e.target as Element | null
      if (el?.closest?.('.popover-trigger, .popover-panel')) return
      setOpen(false)
    }
    function onEsc(e: KeyboardEvent) {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onDown)
    document.addEventListener('keydown', onEsc)
    return () => {
      document.removeEventListener('mousedown', onDown)
      document.removeEventListener('keydown', onEsc)
    }
  }, [open])

  const rect = triggerRef.current?.getBoundingClientRect()
  const panelWidth = 460
  const style: React.CSSProperties = rect
    ? rect.left > panelWidth + 12
      ? {
          position: 'fixed',
          top: rect.top - 4,
          left: rect.left - 8,
          transform: 'translateX(-100%)',
          zIndex: 1000,
          minWidth: 440,
          maxWidth: panelWidth,
        }
      : {
          position: 'fixed',
          top: rect.bottom + 6,
          left: Math.max(8, rect.right - panelWidth),
          zIndex: 1000,
          minWidth: 300,
          maxWidth: panelWidth,
        }
    : { display: 'none' }

  return (
    <>
      <div
        ref={triggerRef}
        className="popover-trigger"
        onClick={(e) => {
          e.stopPropagation()
          setOpen(!open)
        }}
      >
        {trigger}
      </div>
      {open &&
        createPortal(
          <div
            ref={panelRef}
            className="popover-panel"
            style={style}
            // The panel is portaled to <body>, but React synthetic events
            // bubble through the *component* tree, so a click inside the panel
            // would otherwise reach card-level handlers (e.g. Arena's
            // "click = vote"). Stop it here so interacting with the popover
            // (selecting/copying a prompt, clicking 屏蔽) never fires one.
            onClick={(e) => e.stopPropagation()}
          >
            {children}
          </div>,
          document.body,
        )}
    </>
  )
}
