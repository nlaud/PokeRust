import { useEffect, useState } from 'react'
import { fetchSprites } from '../../lib/sprites'

interface SpriteProps {
  species: string
  facing?: 'front' | 'back'
  size?: number
  className?: string
}

/** Gray Pokéball placeholder shown while loading or when a sprite is missing. */
function Placeholder({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" className="opacity-30">
      <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M2 12h7M15 12h7" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  )
}

export default function Sprite({ species, facing = 'front', size = 64, className = '' }: SpriteProps) {
  const [url, setUrl] = useState<string | null>(null)
  const [failed, setFailed] = useState(false)

  useEffect(() => {
    let cancelled = false
    setUrl(null)
    setFailed(false)
    fetchSprites(species).then((urls) => {
      if (cancelled) return
      const resolved = facing === 'front' ? urls.front : (urls.back ?? urls.front)
      if (resolved) setUrl(resolved)
      else setFailed(true)
    })
    return () => {
      cancelled = true
    }
  }, [species, facing])

  if (failed || (!url && !species)) return <Placeholder size={size} />
  if (!url) {
    return (
      <div
        className={`animate-pulse rounded-full bg-subtle ${className}`}
        style={{ width: size, height: size }}
      />
    )
  }
  return (
    <img
      src={url}
      alt={species}
      width={size}
      height={size}
      className={`[image-rendering:pixelated] ${className}`}
      onError={() => setFailed(true)}
    />
  )
}
