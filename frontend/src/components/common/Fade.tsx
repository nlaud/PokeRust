import { useLayoutEffect, useRef, useState, type ReactNode } from 'react'

interface FadeProps {
  /** Content re-fades whenever this changes — e.g. a route path, or `currentPlayer`. */
  fadeKey: string | number
  children: ReactNode
  className?: string
}

/** Quick opacity crossfade keyed on `fadeKey` — used for page/route switches and
 * the P1 <-> P2 perspective flip. On a key change the content dips to 0 opacity
 * and fades back in over ~150ms; the initial mount shows content immediately
 * (no fade-in on first render). CSS-only, no animation library. */
export default function Fade({ fadeKey, children, className = '' }: FadeProps) {
  const [visible, setVisible] = useState(true)
  const prevKey = useRef(fadeKey)

  useLayoutEffect(() => {
    if (prevKey.current === fadeKey) return
    prevKey.current = fadeKey
    setVisible(false)
    const id = requestAnimationFrame(() => setVisible(true))
    return () => cancelAnimationFrame(id)
  }, [fadeKey])

  return (
    <div className={`transition-opacity duration-150 ease-out ${visible ? 'opacity-100' : 'opacity-0'} ${className}`}>
      {children}
    </div>
  )
}
