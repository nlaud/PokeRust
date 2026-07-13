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
  getPlaylist?: () => string[]
  playVideoAt?: (index: number) => void
  mute?: () => void
  unMute?: () => void
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

/**
 * Applies the store's current mute + volume intent to one track's real YouTube
 * player via the IFrame API — deliberately never via ReactPlayer's `muted`/`volume`
 * props. `react-player`'s own `componentDidUpdate` reacts to a `muted` prop
 * transitioning to `false` by calling `unmute()` and then an async
 * `setTimeout(() => setVolume(props.volume))`; since our `volume` prop is a
 * constant `0` (level is imperative, for the crossfade ramp), routing mute through
 * that prop meant every unmute was silently stomped back to volume 0 a tick later.
 * This helper is the single source of truth for both instead.
 *
 * Before the first user gesture (`unlocked` is false) a track is always kept
 * YT-muted regardless of the user's stored mute preference — browsers permit
 * muted autoplay, so this keeps the embed warm and silent — but its *volume level*
 * is still set correctly so there's nothing left to apply once unlocked.
 */
export function applyAudible(track: TrackId) {
  const { muted, volume, activeTrack, crossfading, unlocked } = usePlayer.getState()
  const yt = ytPlayer(track)
  if (!unlocked || muted) {
    yt?.mute?.()
  } else {
    yt?.unMute?.()
  }
  // Crossfade owns the underlying volume while it's running; don't fight the ramp.
  if (!crossfading) {
    yt?.setVolume?.((track === activeTrack ? volume : 0) * 100)
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
  /** Whether a user gesture has unblocked audible (unmuted) playback yet — see
   * `unlock()`. Both tracks always autoplay muted (browsers permit that without a
   * gesture); this flips once we're allowed to actually make sound. */
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
  // Both tracks autoplay muted from mount (browsers always allow muted autoplay),
  // then `unlock()` — fired on the very first user gesture anywhere on the page,
  // see `installUnlockListener` below — flips this and unmutes, so music is
  // audible as early as physically possible instead of only once the user finds
  // and touches the volume slider.
  playing: true,
  unlocked: false,
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
    // Unmuting is itself a user gesture, so use it the same way `unlock()` uses
    // the global first-gesture listener: mark playback unlocked (in case the very
    // first gesture on the page was this button, before any other gesture reached
    // `unlock()`) and re-sync both tracks' real mute/volume state via the YT API —
    // never via ReactPlayer's `muted` prop, see `applyAudible` for why.
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
    // `nextVideo` itself starts playback, which would silently un-pause a user
    // who had explicitly paused — cue the fresh track but immediately re-pause it
    // so `playing: false` survives a manual skip the same way it must survive a
    // crossfade (see `crossfadeTo` below).
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
    // Read the LIVE current volumes (not an assumed 0/full) so an interrupted
    // fade — e.g. a battle ending and a new one starting mid-ramp — resumes
    // smoothly instead of jumping.
    const startSourceVol = (ytPlayer(source)?.getVolume?.() ?? volume * 100) / 100
    const startTargetVol = (ytPlayer(target)?.getVolume?.() ?? 0) / 100

    // Jump the incoming track to a new (shuffled) song rather than resuming
    // wherever it happened to be — both tracks keep playing quietly in the
    // background the whole time (see TrackPlayer), so without this, entering or
    // leaving a battle would resume mid-song instead of feeling like a fresh
    // track change. `nextVideo` also starts playback itself, so it doubles as
    // the "make sure it's playing" call `playVideo` used to be here for.
    const targetYt = ytPlayer(target)
    if (targetYt?.nextVideo) {
      targetYt.nextVideo()
    } else {
      targetYt?.playVideo?.()
    }
    // `nextVideo`/`playVideo` above unconditionally start playback — if the user
    // had explicitly paused (`playing: false`), immediately re-pause the freshly
    // cued target so the paused state survives the track change instead of being
    // silently overridden. The track still jumps to a new song either way; it's
    // just paused on arrival rather than resuming mid-crossfade.
    if (!get().playing) {
      targetYt?.pauseVideo?.()
    }
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

let unlockListenerInstalled = false

/**
 * Browsers permit muted autoplay unconditionally but block unmuted playback until
 * a user gesture occurs on the page. Both tracks start muted+playing (see
 * `MusicPlayer.tsx`'s `TrackPlayer`) so they're already warmed up; this attaches
 * one-shot listeners for the very first gesture *anywhere* on the page — not just
 * the volume slider — and uses it to call `unlock()`, so music becomes audible as
 * early as possible. Safe to call from multiple mounts (e.g. React StrictMode's
 * double-invoked effects); only the first call attaches listeners.
 */
export function installUnlockListener() {
  if (unlockListenerInstalled) return
  unlockListenerInstalled = true
  const handler = () => usePlayer.getState().unlock()
  document.addEventListener('pointerdown', handler, { once: true })
  document.addEventListener('keydown', handler, { once: true })
  document.addEventListener('touchstart', handler, { once: true })
}
