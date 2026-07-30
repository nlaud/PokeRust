import { useEffect, useRef, useState } from 'react'
import ReactPlayer from 'react-player/youtube'
import { useBattle } from '../../store/battleStore'
import { useTracker } from '../../store/trackerStore'
import { applyAudible, installUnlockListener, usePlayer, type TrackId, type YTPlayer } from '../../store/playerStore'

// Shuffle and repeat both YouTube playlists.
// Play ambient music by default.
// Crossfade to battle music during a battle.
const AMBIENT_URL = 'https://www.youtube.com/watch?v=TYdZmrpz7K0'
const AMBIENT_PLAYLIST_ID = 'PL6uHbR5DF8jBFrkhA7-8YQ2K5GlxdeMmP'
const BATTLE_URL = 'https://www.youtube.com/watch?v=3KyqUee895Y'
const BATTLE_PLAYLIST_ID = 'PL6uHbR5DF8jBKITHMx8hwgR0WDz6q7rgt'

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00'
  const m = Math.floor(seconds / 60)
  const s = Math.floor(seconds % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
}

/**
 * Waits for the playlist before it enables shuffle and repeat.
 * Then it selects a random initial track.
 */
function shuffleWhenReady(yt: YTPlayer | undefined, attempt = 0) {
  const playlist = yt?.getPlaylist?.()
  if (playlist && playlist.length > 0) {
    yt?.setShuffle?.(true)
    yt?.setLoop?.(true)
    yt?.playVideoAt?.(Math.floor(Math.random() * playlist.length))
    return
  }
  if (attempt >= 20) return // Stop after the playlist load limit.
  setTimeout(() => shuffleWhenReady(yt, attempt + 1), 250)
}

/** Plays one playlist.
 * CSS hides an inactive player without unmounting it. */
function TrackPlayer({
  track,
  url,
  playlistId,
  visible,
}: {
  track: TrackId
  url: string
  playlistId: string
  visible: boolean
}) {
  const ref = useRef<ReactPlayer>(null)
  const registerRef = usePlayer((s) => s.registerRef)
  const activeTrack = usePlayer((s) => s.activeTrack)
  const playing = usePlayer((s) => s.playing)
  const setProgress = usePlayer((s) => s._setProgress)
  const setDuration = usePlayer((s) => s._setDuration)

  useEffect(() => {
    registerRef(track, ref.current)
    return () => registerRef(track, null)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [track])

  const isActive = activeTrack === track

  return (
    <div className={`absolute inset-0 ${visible ? '' : 'pointer-events-none opacity-0'}`}>
      <ReactPlayer
        ref={ref}
        url={url}
        // Start both tracks in muted autoplay mode.
        // This lets a later crossfade work without a blocked autoplay request.
        // Keep the inactive track at zero volume instead of pausing it.
        playing={playing}
        controls={false}
        loop
        // Keep this value true.
        // `applyAudible` controls mute state through the YouTube player.
        // React Player can otherwise conflict with direct volume control.
        muted
        volume={0} // level is driven imperatively (setVolume/crossfadeTo), not this prop
        width="100%"
        height="100%"
        config={{ playerVars: { listType: 'playlist', list: playlistId } }}
        onReady={(player) => {
          const yt = player.getInternalPlayer() as YTPlayer | undefined
          shuffleWhenReady(yt)
          // Set the initial track volume here.
          // `crossfadeTo` changes volume only after a track change.
          applyAudible(track)
        }}
        onProgress={(state) => isActive && setProgress(track, state.playedSeconds)}
        onDuration={(d) => isActive && setDuration(track, d)}
      />
    </div>
  )
}

const volumeIcon = (muted: boolean) => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M11 5 6 9H2v6h4l5 4V5z" />
    {muted ? <path d="m22 9-6 6M16 9l6 6" /> : <path d="M15.5 8.5a5 5 0 0 1 0 7M19 5a10 10 0 0 1 0 14" />}
  </svg>
)
const playIcon = (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
    <path d="M8 5v14l11-7z" />
  </svg>
)
const pauseIcon = (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="currentColor">
    <path d="M6 5h4v14H6zM14 5h4v14h-4z" />
  </svg>
)
const skipIcon = (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor">
    <path d="M6 5v14l8-7zM16 5h2v14h-2z" />
  </svg>
)

export default function MusicPlayer() {
  const volume = usePlayer((s) => s.volume)
  const muted = usePlayer((s) => s.muted)
  const playing = usePlayer((s) => s.playing)
  const playedSeconds = usePlayer((s) => s.playedSeconds)
  const duration = usePlayer((s) => s.duration)
  const activeTrack = usePlayer((s) => s.activeTrack)
  const setVolume = usePlayer((s) => s.setVolume)
  const toggleMute = usePlayer((s) => s.toggleMute)
  const togglePlay = usePlayer((s) => s.togglePlay)
  const seekTo = usePlayer((s) => s.seekTo)
  const skip = usePlayer((s) => s.skip)
  const crossfadeTo = usePlayer((s) => s.crossfadeTo)

  // Listen for the first user action when the application starts.
  // This action permits audible playback.
  useEffect(() => {
    installUnlockListener()
  }, [])

  // Keep local progress while the user drags the scrubber.
  const [scrubValue, setScrubValue] = useState<number | null>(null)

  // Treat only the normal battle phase as active combat.
  // The reactive selector also follows an asynchronous session restore.
  const fighting = useBattle(
    (s) =>
      s.battleId != null &&
      s.view != null &&
      (s.view.phase === 'normal' || s.view.phase === 'selfSwitch' || s.view.phase === 'replacement'),
  )
  // Treat a tracker session as active combat.
  // Tracker views always use the `normal` phase.
  const trackerFighting = useTracker((s) => s.trackerId != null && s.view != null)
  useEffect(() => {
    crossfadeTo(fighting || trackerFighting ? 'battle' : 'ambient')
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fighting, trackerFighting])

  const shownTime = scrubValue ?? playedSeconds

  const commitScrub = (e: React.SyntheticEvent<HTMLInputElement>) => {
    seekTo(Number(e.currentTarget.value))
    setScrubValue(null)
  }

  return (
    <section className="mt-auto pt-4">
      <h3 className="mb-2 text-sm font-medium text-ink-muted">Music</h3>

      <div className="mb-2 flex items-center gap-2">
        <button
          onClick={toggleMute}
          className="text-ink-muted hover:text-ink"
          aria-label={muted ? 'Unmute' : 'Mute'}
        >
          {volumeIcon(muted)}
        </button>
        <input
          type="range"
          min={0}
          max={1}
          step={0.01}
          value={volume}
          onChange={(e) => setVolume(Number(e.target.value))}
          className={`h-1.5 flex-1 accent-primary ${muted ? 'opacity-50' : ''}`}
          aria-label="Volume"
        />
      </div>

      <div className="relative aspect-video w-full overflow-hidden rounded-card bg-black">
        <TrackPlayer track="ambient" url={AMBIENT_URL} playlistId={AMBIENT_PLAYLIST_ID} visible={activeTrack === 'ambient'} />
        <TrackPlayer track="battle" url={BATTLE_URL} playlistId={BATTLE_PLAYLIST_ID} visible={activeTrack === 'battle'} />

        {/* Block YouTube controls and show the application playback controls. */}
        <div className="absolute inset-0 flex flex-col justify-end bg-gradient-to-t from-black/70 via-transparent to-transparent p-2">
          <div className="mb-1 flex items-center justify-center gap-4 text-white">
            <button onClick={togglePlay} aria-label={playing ? 'Pause' : 'Play'}>
              {playing ? pauseIcon : playIcon}
            </button>
            <button onClick={skip} aria-label="Skip to next track">
              {skipIcon}
            </button>
          </div>
          <div className="flex items-center gap-2 text-[10px] text-white">
            <span>{formatTime(shownTime)}</span>
            <input
              type="range"
              min={0}
              max={duration || 0}
              step={1}
              value={shownTime}
              onChange={(e) => setScrubValue(Number(e.target.value))}
              onMouseUp={commitScrub}
              onTouchEnd={commitScrub}
              className="h-1 flex-1 accent-primary"
              aria-label="Seek"
            />
            <span>{formatTime(duration)}</span>
          </div>
        </div>
      </div>
    </section>
  )
}
