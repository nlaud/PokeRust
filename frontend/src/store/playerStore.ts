import { create } from 'zustand'
import type ReactPlayer from 'react-player'

const PLAYER_KEY = 'pokerust.player.v1'

export type TrackId = 'ambient' | 'battle'

interface PlayerSettings {
  volume: number // 0–1
  muted: boolean
}

function loadPlayerSettings(): PlayerSettings {
  try {
    const raw = localStorage.getItem(PLAYER_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<PlayerSettings>
      return { volume: parsed.volume ?? 0.5, muted: parsed.muted ?? false }
    }
  } catch {
    // fall through to defaults
  }
  return { volume: 0.5, muted: false }
}

function persist(settings: PlayerSettings) {
  localStorage.setItem(PLAYER_KEY, JSON.stringify(settings))
}

// Player refs live OUTSIDE Zustand state, in a module-level object — imperative
// handles shouldn't flow through React state (stale-closure risk, needless
// re-renders), and the crossfade rAF loop below must keep running even while
// nothing is subscribed to the store (e.g. the Settings sidebar is closed).
const playerRefs: Record<TrackId, ReactPlayer | null> = { ambient: null, battle: null }

/** The subset of the real YouTube `YT.Player` instance's official IFrame API this
 * app calls — reached via react-player's `getInternalPlayer()` escape hatch, not an
 * undocumented workaround. Exported so `MusicPlayer.tsx`'s `onReady` (which gets a
 * player instance directly, not through `ytPlayer()`) can share the same typing. */
export interface YTPlayer {
  setShuffle?: (shuffle: boolean) => void
  setLoop?: (loop: boolean) => void
  setVolume?: (volume: number) => void
  getVolume?: () => number
  playVideo?: () => void
  pauseVideo?: () => void
  nextVideo?: () => void
}

/** The real YouTube `YT.Player` instance behind a track, or `null` if the player
 * hasn't mounted/readied yet. */
function ytPlayer(track: TrackId): YTPlayer | null {
  const ref = playerRefs[track]
  if (!ref) return null
  try {
    return ref.getInternalPlayer() as YTPlayer
  } catch {
    return null
  }
}

const CROSSFADE_MS = 2500
// Cancel token + rAF handle for the crossfade loop, module-level so a new
// crossfade cleanly aborts one already in flight (e.g. a battle ending and a new
// one starting in quick succession) — never two ramps racing each other.
let rampToken = 0
let rampHandle = 0

interface PlayerStore extends PlayerSettings {
  activeTrack: TrackId
  crossfading: boolean
  playing: boolean
  playedSeconds: number
  duration: number
  setVolume: (volume: number) => void
  toggleMute: () => void
  togglePlay: () => void
  seekTo: (seconds: number) => void
  skip: () => void
  crossfadeTo: (target: TrackId) => void
  registerRef: (track: TrackId, ref: ReactPlayer | null) => void
  _setProgress: (track: TrackId, seconds: number) => void
  _setDuration: (track: TrackId, duration: number) => void
}

const initial = loadPlayerSettings()

export const usePlayer = create<PlayerStore>((set, get) => ({
  ...initial,
  activeTrack: 'ambient',
  crossfading: false,
  // Browsers block autoplay of unmuted audio without a prior user gesture, so the
  // ambient player may not actually be audible until the user first interacts with
  // the page (e.g. opens Settings and toggles play/unmute) — expected, not a bug.
  playing: true,
  playedSeconds: 0,
  duration: 0,

  setVolume: (volume) => {
    persist({ volume, muted: get().muted })
    set({ volume })
    // Crossfade owns the underlying volume while it's running; otherwise apply
    // the new level to whichever track is currently audible.
    if (!get().crossfading) {
      const yt = ytPlayer(get().activeTrack)
      yt?.setVolume?.(volume * 100)
    }
  },

  toggleMute: () => {
    const muted = !get().muted
    persist({ volume: get().volume, muted })
    set({ muted })
  },

  togglePlay: () => set((s) => ({ playing: !s.playing })),

  seekTo: (seconds) => {
    playerRefs[get().activeTrack]?.seekTo(seconds, 'seconds')
  },

  skip: () => {
    ytPlayer(get().activeTrack)?.nextVideo?.()
  },

  registerRef: (track, ref) => {
    playerRefs[track] = ref
  },

  _setProgress: (track, seconds) => {
    if (track === get().activeTrack) set({ playedSeconds: seconds })
  },
  _setDuration: (track, duration) => {
    if (track === get().activeTrack) set({ duration })
  },

  crossfadeTo: (target) => {
    const { activeTrack, crossfading, volume } = get()
    if (target === activeTrack && !crossfading) return

    rampToken += 1
    const myToken = rampToken
    cancelAnimationFrame(rampHandle)

    const source = activeTrack
    // Read the LIVE current volumes (not an assumed 0/full) so an interrupted
    // fade — e.g. a battle ending and a new one starting mid-ramp — resumes
    // smoothly instead of jumping.
    const startSourceVol = (ytPlayer(source)?.getVolume?.() ?? volume * 100) / 100
    const startTargetVol = (ytPlayer(target)?.getVolume?.() ?? 0) / 100

    ytPlayer(target)?.playVideo?.()
    // Flip immediately: the visible video should match the incoming music as it
    // fades in, not wait until the fade completes.
    set({ activeTrack: target, crossfading: true })

    const startTime = performance.now()
    const step = () => {
      if (rampToken !== myToken) return // superseded by a newer crossfade
      const elapsed = performance.now() - startTime
      const t = Math.min(1, elapsed / CROSSFADE_MS)
      const targetVol = startTargetVol + (get().volume - startTargetVol) * t
      const sourceVol = startSourceVol * (1 - t)
      // Re-fetch each frame (cheap) rather than snapshotting once, so a target
      // player that wasn't ready yet at crossfade-start self-heals once it is.
      ytPlayer(target)?.setVolume?.(targetVol * 100)
      ytPlayer(source)?.setVolume?.(sourceVol * 100)
      if (t < 1) {
        rampHandle = requestAnimationFrame(step)
      } else {
        ytPlayer(source)?.pauseVideo?.()
        set({ crossfading: false })
      }
    }
    rampHandle = requestAnimationFrame(step)
  },
}))
