import { useEffect, useState } from 'react'
import { cachedImageUrl, fetchSprites } from '../../lib/sprites'

interface SpriteProps {
  species: string
  facing?: 'front' | 'back'
  size?: number
  className?: string
}

/** Shows a gray Poké Ball after all sprite candidates return HTTP 404. */
function Placeholder({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" className="opacity-30">
      <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M2 12h7M15 12h7" stroke="currentColor" strokeWidth="1.5" />
      <circle cx="12" cy="12" r="3" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  )
}

/** Shows a spinner during a sprite load or retry. */
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

// `fetchSprites` first retries short network failures.
// These longer delays handle failures that remain after those retries.
const MAX_FETCH_RETRIES = 5
const FETCH_RETRY_BASE_MS = 1000
const MAX_IMG_RETRIES = 2

export default function Sprite({ species, facing = 'front', size = 64, className = '' }: SpriteProps) {
  const [url, setUrl] = useState<string | null>(null)
  const [failed, setFailed] = useState(false)
  const [loaded, setLoaded] = useState(false)
  const [useProxy, setUseProxy] = useState(true)

  useEffect(() => {
    let cancelled = false
    let attempt = 0
    let retryTimer: ReturnType<typeof setTimeout> | undefined

    setUrl(null)
    setFailed(false)

    function attemptFetch() {
      fetchSprites(species).then(
        (urls) => {
          if (cancelled) return
          const resolved = facing === 'front' ? urls.front : (urls.back ?? urls.front)
          // A null result means that each candidate returned HTTP 404.
          // Show the placeholder.
          if (resolved) setUrl(resolved)
          else setFailed(true)
        },
        () => {
          // Keep the spinner visible after a temporary failure.
          // Retry without a page reload.
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

  // Load the image bytes before the component hides the spinner.
  // A detached `Image` stores the data in the browser cache.
  // The visible image then uses the cached data.
  useEffect(() => {
    if (!url) return
    const resolvedUrl = url
    let cancelled = false
    let retries = 0
    let proxy = true
    setLoaded(false)
    setUseProxy(true)

    function attemptLoad() {
      const probe = new Image()
      probe.onload = () => {
        if (!cancelled) setLoaded(true)
      }
      probe.onerror = () => {
        if (cancelled) return
        // Retry a temporary image-load failure.
        // After the local proxy retries, try the direct URL once.
        // An HTTP 404 already produces a null resolved value.
        if (retries < MAX_IMG_RETRIES) {
          retries += 1
          setTimeout(attemptLoad, 300 * retries)
        } else if (proxy) {
          proxy = false
          setUseProxy(false)
          attemptLoad()
        } else {
          setFailed(true)
        }
      }
      probe.src = proxy ? cachedImageUrl(resolvedUrl) : resolvedUrl
    }

    attemptLoad()
    return () => {
      cancelled = true
    }
  }, [url])

  if (failed || (!url && !species)) return <Placeholder size={size} />
  if (!url || !loaded) return <Spinner size={size} className={className} />
  return (
    <img
      src={useProxy ? cachedImageUrl(url) : url}
      alt={species}
      width={size}
      height={size}
      className={`[image-rendering:pixelated] ${className}`}
    />
  )
}
