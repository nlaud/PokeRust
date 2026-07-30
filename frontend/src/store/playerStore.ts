import { create } from 'zustand'
import type ReactPlayer from 'react-player/youtube'

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
    // Use the defaults after an invalid stored value.
  }
  return { volume: 0.5, muted: false }
}

function persist(settings: PlayerSettings) {
  localStorage.setItem(PLAYER_KEY, JSON.stringify(settings))
}

// Store player references outside Zustand state.
// Imperative handles do not need React updates.
// The crossfade loop also runs without a store subscriber.
const playerRefs: Record<TrackId, ReactPlayer | null> = { ambient: null, battle: null }

/** YouTube player methods that this application uses.
 * `MusicPlayer.tsx` also uses this type for its ready callback. */
export interface YTPlayer {
  setShuffle?: (shuffle: boolean) => void
  setLoop?: (loop: boolean) => void
  setVolume?: (volume: number) => void
  getVolume?: () => number
  playVideo?: () => void
  pauseVideo?: () => void
  nextVideo?: () => void
  getPlaylist?: () => string[]
  playVideoAt?: (index: number) => void
  mute?: () => void
  unMute?: () => void
}

/** Returns the YouTube player, or `null` before it is ready. */
function ytPlayer(track: TrackId): YTPlayer | null {
  const ref = playerRefs[track]
  if (!ref) return null
  try {
    return ref.getInternalPlayer() as YTPlayer
  } catch {
    return null
  }
}

/**
 * Applies the current mute and volume values through the YouTube API.
 * React Player properties can overwrite direct crossfade volume changes.
 * Before the first user action, the function keeps the track muted.
 * It still sets the correct volume for later playback.
 */
export function applyAudible(track: TrackId) {
  const { muted, volume, activeTrack, crossfading, unlocked } = usePlayer.getState()
  const yt = ytPlayer(track)
  if (!unlocked || muted) {
    yt?.mute?.()
  } else {
    yt?.unMute?.()
  }
  // Do not change track volume during a crossfade.
  if (!crossfading) {
    yt?.setVolume?.((track === activeTrack ? volume : 0) * 100)
  }
}

const CROSSFADE_MS = 2500
// These module values identify the active crossfade.
// A new crossfade cancels the old crossfade.
let rampToken = 0
let rampHandle = 0

interface PlayerStore extends PlayerSettings {
  activeTrack: TrackId
  crossfading: boolean
  playing: boolean
  /** True after a user action permits audible playback. */
  unlocked: boolean
  playedSeconds: number
  duration: number
  setVolume: (volume: number) => void
  toggleMute: () => void
  togglePlay: () => void
  seekTo: (seconds: number) => void
  skip: () => void
  crossfadeTo: (target: TrackId) => void
  unlock: () => void
  registerRef: (track: TrackId, ref: ReactPlayer | null) => void
  _setProgress: (track: TrackId, seconds: number) => void
  _setDuration: (track: TrackId, duration: number) => void
}

const initial = loadPlayerSettings()

export const usePlayer = create<PlayerStore>((set, get) => ({
  ...initial,
  activeTrack: 'ambient',
  crossfading: false,
  // Start both tracks with muted autoplay.
  // The first user action calls `unlock` and permits audible playback.
  playing: true,
  unlocked: false,
  playedSeconds: 0,
  duration: 0,

  setVolume: (volume) => {
    persist({ volume, muted: get().muted })
    set({ volume })
    // During a crossfade, let the crossfade control volume.
    // Otherwise, apply the new volume to the active track.
    if (!get().crossfading) {
      const yt = ytPlayer(get().activeTrack)
      yt?.setVolume?.(volume * 100)
    }
  },

  toggleMute: () => {
    const muted = !get().muted
    persist({ volume: get().volume, muted })
    // An unmute action also permits audible playback.
    // Update both players through the YouTube API.
    // Do not use the React Player `muted` property.
    set({ muted, unlocked: true })
    applyAudible('ambient')
    applyAudible('battle')
    if (!muted && !get().crossfading) {
      ytPlayer(get().activeTrack)?.playVideo?.()
    }
  },

  togglePlay: () => set((s) => ({ playing: !s.playing })),

  seekTo: (seconds) => {
    playerRefs[get().activeTrack]?.seekTo(seconds, 'seconds')
  },

  skip: () => {
    const yt = ytPlayer(get().activeTrack)
    yt?.nextVideo?.()
    // `nextVideo` starts playback.
    // Pause the new track when the user had paused playback.
    if (!get().playing) {
      yt?.pauseVideo?.()
    }
  },

  unlock: () => {
    if (get().unlocked) return
    set({ unlocked: true })
    applyAudible('ambient')
    applyAudible('battle')
    if (!get().muted && !get().crossfading) {
      ytPlayer(get().activeTrack)?.playVideo?.()
    }
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
    // Read the current volumes before a crossfade.
    // This prevents a jump when a new crossfade interrupts the old one.
    const startSourceVol = (ytPlayer(source)?.getVolume?.() ?? volume * 100) / 100
    const startTargetVol = (ytPlayer(target)?.getVolume?.() ?? 0) / 100

    // Select a new shuffled song for the incoming track.
    // `nextVideo` also starts the track.
    const targetYt = ytPlayer(target)
    if (targetYt?.nextVideo) {
      targetYt.nextVideo()
    } else {
      targetYt?.playVideo?.()
    }
    // Pause the new song when the user had paused playback.
    // This preserves the pause state across a track change.
    if (!get().playing) {
      targetYt?.pauseVideo?.()
    }
    // Show the incoming video when its crossfade starts.
    set({ activeTrack: target, crossfading: true })

    const startTime = performance.now()
    const step = () => {
      if (rampToken !== myToken) return // superseded by a newer crossfade
      const elapsed = performance.now() - startTime
      const t = Math.min(1, elapsed / CROSSFADE_MS)
      const targetVol = startTargetVol + (get().volume - startTargetVol) * t
      const sourceVol = startSourceVol * (1 - t)
      // Get the player references during each frame.
      // This includes a player that becomes ready after the crossfade starts.
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

let unlockListenerInstalled = false

/**
 * Installs one-time listeners for the first user action.
 * The action calls `unlock` and permits audible playback.
 * Repeated calls do not install more listeners.
 */
export function installUnlockListener() {
  if (unlockListenerInstalled) return
  unlockListenerInstalled = true
  const handler = () => usePlayer.getState().unlock()
  document.addEventListener('pointerdown', handler, { once: true })
  document.addEventListener('keydown', handler, { once: true })
  document.addEventListener('touchstart', handler, { once: true })
}
