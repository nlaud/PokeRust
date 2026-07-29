import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'

interface TooltipPosition {
  left: number
  top: number
}

export default function Tooltip({
  content,
  children,
  className = '',
}: {
  content?: string
  children: ReactNode
  className?: string
}) {
  const id = useId()
  const anchorRef = useRef<HTMLSpanElement>(null)
  const tooltipRef = useRef<HTMLSpanElement>(null)
  const [open, setOpen] = useState(false)
  const [position, setPosition] = useState<TooltipPosition | null>(null)
  const hide = () => {
    setOpen(false)
    setPosition(null)
  }

  useEffect(() => {
    if (!open || !content) return

    const place = () => {
      const anchor = anchorRef.current
      const tooltip = tooltipRef.current
      if (!anchor || !tooltip) return

      const anchorRect = anchor.getBoundingClientRect()
      const tooltipRect = tooltip.getBoundingClientRect()
      const margin = 8
      const centered = anchorRect.left + anchorRect.width / 2 - tooltipRect.width / 2
      const left = Math.min(
        Math.max(centered, margin),
        Math.max(margin, window.innerWidth - tooltipRect.width - margin),
      )
      const roomBelow = window.innerHeight - anchorRect.bottom
      const top =
        roomBelow >= tooltipRect.height + margin * 2
          ? anchorRect.bottom + margin
          : anchorRect.top - tooltipRect.height - margin

      setPosition({ left, top: Math.max(margin, top) })
    }

    place()
    window.addEventListener('resize', place)
    window.addEventListener('scroll', place, true)
    return () => {
      window.removeEventListener('resize', place)
      window.removeEventListener('scroll', place, true)
    }
  }, [content, open])

  if (!content) {
    return <span className={className}>{children}</span>
  }

  return (
    <>
      <span
        ref={anchorRef}
        tabIndex={0}
        aria-describedby={open ? id : undefined}
        className={className}
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={hide}
        onPointerEnter={() => setOpen(true)}
        onPointerLeave={hide}
        onFocus={() => setOpen(true)}
        onBlur={hide}
        onClick={() => setOpen(true)}
      >
        {children}
      </span>
      {open &&
        createPortal(
          <span
            ref={tooltipRef}
            id={id}
            role="tooltip"
            style={{
              left: position?.left ?? 0,
              top: position?.top ?? 0,
              visibility: position ? 'visible' : 'hidden',
            }}
            className="pointer-events-none fixed z-[100] w-max max-w-72 whitespace-pre-line rounded-card border border-default bg-surface px-2.5 py-2 text-left text-[11px] font-normal normal-case leading-relaxed tracking-normal text-ink shadow-lg no-underline"
          >
            {content}
          </span>,
          document.body,
        )}
    </>
  )
}
