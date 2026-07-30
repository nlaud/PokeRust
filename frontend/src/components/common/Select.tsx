import { useEffect, useRef, useState } from 'react'

export interface SelectOption {
  value: string
  label: string
}

/**
 * Shows a styled list box.
 * An outside click or Escape closes it.
 * Enter, Space, and arrow keys control the selection.
 */
export default function Select({
  value,
  options,
  onChange,
  placeholder = 'Select…',
  disabled = false,
}: {
  value: string
  options: SelectOption[]
  onChange: (value: string) => void
  placeholder?: string
  disabled?: boolean
}) {
  const [open, setOpen] = useState(false)
  const [highlight, setHighlight] = useState(0)
  const rootRef = useRef<HTMLDivElement>(null)

  const selected = options.find((o) => o.value === value)

  useEffect(() => {
    if (!open) return
    const onPointerDown = (e: PointerEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false)
    }
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      document.removeEventListener('keydown', onKeyDown)
    }
  }, [open])

  const openMenu = () => {
    if (disabled) return
    setHighlight(Math.max(0, options.findIndex((o) => o.value === value)))
    setOpen((o) => !o)
  }

  const pick = (option: SelectOption) => {
    onChange(option.value)
    setOpen(false)
  }

  const onTriggerKeyDown = (e: React.KeyboardEvent) => {
    if (disabled) return
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      if (!open) {
        openMenu()
        return
      }
      setHighlight((h) => {
        const next = e.key === 'ArrowDown' ? h + 1 : h - 1
        return Math.min(options.length - 1, Math.max(0, next))
      })
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      if (open && options[highlight]) pick(options[highlight])
      else openMenu()
    }
  }

  return (
    <div ref={rootRef} className="relative">
      <button
        type="button"
        onClick={openMenu}
        onKeyDown={onTriggerKeyDown}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        className="flex w-full items-center justify-between rounded-card border border-subtle bg-surface px-3 py-2 text-left text-sm outline-none transition-colors focus:border-primary disabled:opacity-50"
      >
        <span className={selected ? '' : 'text-ink-muted'}>{selected?.label ?? placeholder}</span>
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`shrink-0 text-ink-muted transition-transform duration-200 ${open ? 'rotate-180' : ''}`}
        >
          <path d="m6 9 6 6 6-6" />
        </svg>
      </button>

      <div
        className={`absolute inset-x-0 top-full z-30 mt-1 origin-top rounded-card border border-subtle bg-card shadow-lg transition-all duration-150 ease-out ${
          open
            ? 'pointer-events-auto translate-y-0 scale-100 opacity-100'
            : 'pointer-events-none -translate-y-1 scale-95 opacity-0'
        }`}
        role="listbox"
      >
        <div className="max-h-60 overflow-y-auto p-1">
          {options.length === 0 && (
            <div className="px-2.5 py-1.5 text-sm text-ink-muted">No options</div>
          )}
          {options.map((option, i) => (
            <button
              key={option.value}
              type="button"
              role="option"
              aria-selected={option.value === value}
              onClick={() => pick(option)}
              onMouseEnter={() => setHighlight(i)}
              className={`block w-full rounded-md px-2.5 py-1.5 text-left text-sm transition-colors ${
                i === highlight ? 'bg-primary-soft text-primary' : ''
              } ${option.value === value ? 'font-semibold' : ''}`}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  )
}
