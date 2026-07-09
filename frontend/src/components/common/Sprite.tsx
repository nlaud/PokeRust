import { useEffect, useRef, useState } from 'react'
import { fetchSprites } from '../../lib/sprites'

interface SpriteProps {
  species: string
  facing?: 'front' | 'back'
  size?: number
  className?: string
}

/** Gray Pokéball placeholder shown only once a sprite is confirmed missing
 *  (a clean 404 through every fallback) — never for a merely-slow load. */
function Placeholder({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" className="opacity-30">
      <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M2 12h7M15 12h7" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  )
}

/** Spinner shown while a sprite is loading or a failed lookup is being retried. */
function Spinner({ size, className = '' }: { size: number; className?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      className={`animate-spin text-ink-muted opacity-50 ${className}`}
    >
      <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeWidth="2" strokeOpacity="0.25" />
      <path d="M21 12a9 9 0 0 0-9-9" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
    </svg>
  )
}

// fetchSprites() already retries transient network failures internally
// (see lib/sprites.ts) before it ever rejects. A rejection reaching this
// component means that whole internal retry budget was exhausted, so these
// retries are spaced further apart — this is for outages measured in
// seconds, not the odd dropped request.
const MAX_FETCH_RETRIES = 5
const FETCH_RETRY_BASE_MS = 1000
const MAX_IMG_RETRIES = 2

export default function Sprite({ species, facing = 'front', size = 64, className = '' }: SpriteProps) {
  const [url, setUrl] = useState<string | null>(null)
  const [failed, setFailed] = useState(false)
  const imgRetries = useRef(0)

  useEffect(() => {
    let cancelled = false
    let attempt = 0
    let retryTimer: ReturnType<typeof setTimeout> | undefined

    setUrl(null)
    setFailed(false)
    imgRetries.current = 0

    function attemptFetch() {
      fetchSprites(species).then(
        (urls) => {
          if (cancelled) return
          const resolved = facing === 'front' ? urls.front : (urls.back ?? urls.front)
          // A resolved-but-null result means every candidate in the
          // resolution chain came back a clean 404 — this sprite genuinely
          // doesn't exist, so go straight to the placeholder.
          if (resolved) setUrl(resolved)
          else setFailed(true)
        },
        () => {
          // fetchSprites rejected — a transient failure that survived its
          // own internal retries. Keep the spinner up and retry here too,
          // so a sprite that lost the race during a page-load burst still
          // eventually appears without the user reloading the page.
          if (cancelled) return
          if (attempt >= MAX_FETCH_RETRIES) {
            setFailed(true)
            return
          }
          attempt += 1
          const backoff = FETCH_RETRY_BASE_MS * 2 ** (attempt - 1)
          retryTimer = setTimeout(attemptFetch, backoff + Math.random() * backoff * 0.5)
        },
      )
    }

    attemptFetch()
    return () => {
      cancelled = true
      if (retryTimer) clearTimeout(retryTimer)
    }
  }, [species, facing])

  if (failed || (!url && !species)) return <Placeholder size={size} />
  if (!url) return <Spinner size={size} className={className} />
  return (
    <img
      src={url}
      alt={species}
      width={size}
      height={size}
      className={`[image-rendering:pixelated] ${className}`}
      onError={() => {
        // The image URL resolved fine but the actual GitHub-hosted sprite
        // occasionally drops mid-load under burst too. Force the <img> to
        // re-mount with the same src (a couple of times) before conceding —
        // a genuine 404 here would already have been caught upstream as a
        // `resolved === null` case, not this onError.
        if (imgRetries.current < MAX_IMG_RETRIES) {
          imgRetries.current += 1
          const retryUrl = url
          setUrl(null)
          setTimeout(() => setUrl(retryUrl), 300 * imgRetries.current)
        } else {
          setFailed(true)
        }
      }}
    />
  )
}
