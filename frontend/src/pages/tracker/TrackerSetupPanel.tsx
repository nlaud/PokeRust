import { useEffect, useState } from 'react'
import { listSpecies } from '../../api/client'
import Select from '../../components/common/Select'
import { CATALOG } from '../../lib/items'
import { favoritesFirst, loadFormats, loadTeams, type StoredFormat } from '../../lib/storage'
import { useTracker } from '../../store/trackerStore'
import SpeciesPicker from './SpeciesPicker'

/** Mirrors `pages/simulate/SetupPanel.tsx`'s `legalItemsFor`. */
function legalItemsFor(format: StoredFormat): string[] {
  const banned = new Set(format.bannedItems)
  return CATALOG.filter((item) => !banned.has(item.name)).map((item) => item.name)
}

const INFO_MODE_OPTIONS = [
  { value: 'closedSheet', label: 'Closed Team Sheet' },
  { value: 'openSheet', label: 'Open Team Sheet' },
  { value: 'openSheetNatures', label: 'Open Team Sheet + Natures' },
] as const
type TrackerInfoMode = (typeof INFO_MODE_OPTIONS)[number]['value']

/** What the opponent field expects under each information mode — shown as
 * both the field's placeholder and a hint line, so the pasted-in text always
 * matches what the selected mode can actually see. */
const OPPONENT_HINT: Record<TrackerInfoMode, string> = {
  closedSheet:
    "Just species — pick or paste the opponent's roster; item/ability/moves/nature stay hidden either way.",
  openSheet:
    'A full teamsheet: species @ item, ability, and moves are visible under Open Team Sheet — nature and EVs/IVs stay hidden.',
  openSheetNatures:
    'A full teamsheet: species @ item, ability, moves, and nature are all visible under Open Team Sheet + Natures — EVs/IVs stay hidden.',
}
const OPPONENT_PLACEHOLDER: Record<TrackerInfoMode, string> = {
  closedSheet: 'Garchomp, Landorus, Incineroar, Rillaboom, Flutter Mane, Urshifu',
  openSheet: 'Garchomp @ Rocky Helmet\nAbility: Rough Skin\n- Earthquake\n- Stealth Rock\n...',
  openSheetNatures:
    'Garchomp @ Rocky Helmet\nAbility: Rough Skin\nJolly Nature\n- Earthquake\n- Stealth Rock\n...',
}

/**
 * Tracker mode's start flow — mirrors `SetupPanel.tsx`'s battle-start flow
 * (ruleset + information mode pickers, reused `Select`/`favoritesFirst`), but
 * in place of a second team select: a text box for the opponent (a teamsheet
 * or comma-separated species — the server normalizes the latter). There's no
 * lead picker here — leads for BOTH sides are conveyed by a
 * `leads p … o …` tracker-text event once the session starts (see the
 * server's `tracker.rs` module doc: a session begins fully benched on both
 * sides).
 */
export default function TrackerSetupPanel() {
  const [teams] = useState(loadTeams)
  const [formats] = useState(loadFormats)
  const { create, busy, error, clearError } = useTracker()

  // Default to favorited teams first (falling back to storage order) rather
  // than raw `teams[0]`, so a starred team is what's pre-selected — mirrors
  // `SetupPanel.tsx`'s default-selection behavior.
  const sortedTeams = favoritesFirst(teams)
  const defaultFormat = formats.find((f) => f.id === 'champions-s2-doubles') ?? formats[0]
  const [formatId, setFormatId] = useState(defaultFormat?.id ?? '')
  const [teamId, setTeamId] = useState(sortedTeams[0]?.id ?? '')
  const [informationMode, setInformationMode] = useState<TrackerInfoMode>('closedSheet')
  const [opponent, setOpponent] = useState('')
  // Closed sheet only ever names species, so it gets a validated chip picker
  // instead of the textarea (see `SpeciesPicker`). Kept as separate state from
  // `opponent` so switching modes back and forth doesn't destroy either one.
  const [opponentSpecies, setOpponentSpecies] = useState<string[]>([])
  const [speciesCatalog, setSpeciesCatalog] = useState<string[]>([])
  const [speciesLoading, setSpeciesLoading] = useState(true)

  // One fetch for the whole page — the list is static dex data, and the picker
  // is unusable without it.
  useEffect(() => {
    let cancelled = false
    listSpecies()
      .then((res) => {
        if (!cancelled) setSpeciesCatalog(res.species)
      })
      .catch(() => {
        // Non-fatal: the picker degrades to accepting nothing, and the user can
        // switch to a sheet mode. The real failure surfaces on submit.
      })
      .finally(() => {
        if (!cancelled) setSpeciesLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [])

  const format = formats.find((f) => f.id === formatId)
  const team = teams.find((t) => t.id === teamId)
  const usesPicker = informationMode === 'closedSheet'
  // The server requires at least `broughtPerSide` parseable Pokemon
  // (`tracker.rs::create_tracker`), so gate on exactly that rather than a fixed
  // 6 — a real closed sheet shows the whole roster, but a Bo3 game 2 might not.
  const minSpecies = format?.broughtPokemon ?? 0
  const opponentReady = usesPicker
    ? opponentSpecies.length >= minSpecies && minSpecies > 0
    : opponent.trim().length > 0
  const ready = format && team && opponentReady

  const start = () => {
    if (!ready || !format || !team) return
    clearError()
    void create({
      myTeam: team.sheet,
      // `normalize_opponent_text` already splits a single comma-separated line
      // into per-mon blocks, so the picker needs no API change of its own.
      opponent: usesPicker ? opponentSpecies.join(', ') : opponent,
      activePerSide: format.activePokemon,
      broughtPerSide: format.broughtPokemon,
      forceMaxIvs: format.forceMaxIvs,
      informationMode,
      legalItems: legalItemsFor(format),
    })
  }

  const formatOptions = favoritesFirst(formats).map((f) => ({ value: f.id, label: f.name }))
  const teamOptions = sortedTeams.map((t) => ({ value: t.id, label: t.name }))

  return (
    <div className="mx-auto max-w-lg p-6">
      <div className="glass mt-12 rounded-card p-6 shadow-md">
        <h1 className="mb-6 text-xl font-semibold">New Tracker Session</h1>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Ruleset</span>
          <Select value={formatId} options={formatOptions} onChange={setFormatId} />
        </label>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Your team</span>
          <Select value={teamId} options={teamOptions} onChange={setTeamId} />
        </label>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Information mode</span>
          <Select
            value={informationMode}
            options={[...INFO_MODE_OPTIONS]}
            onChange={(v) => setInformationMode(v as TrackerInfoMode)}
          />
        </label>

        <div className="mb-6 block text-sm">
          <span className="mb-1 block font-medium">Opponent</span>
          {usesPicker ? (
            <SpeciesPicker
              value={opponentSpecies}
              catalog={speciesCatalog}
              minCount={minSpecies}
              loading={speciesLoading}
              onChange={setOpponentSpecies}
            />
          ) : (
            <textarea
              value={opponent}
              onChange={(e) => setOpponent(e.target.value)}
              rows={7}
              spellCheck={false}
              placeholder={OPPONENT_PLACEHOLDER[informationMode]}
              className="w-full resize-none rounded-card border border-subtle bg-card px-3 py-2 font-mono text-xs"
            />
          )}
          <span className="mt-1 block text-[11px] text-ink-muted">{OPPONENT_HINT[informationMode]}</span>
        </div>

        {teams.length === 0 && (
          <p className="mb-4 text-sm text-warning">
            No teams yet — create one on the Teams page first.
          </p>
        )}
        {error && <p className="mb-4 text-sm text-danger">{error}</p>}

        <button
          onClick={start}
          disabled={!ready || busy}
          className="lift w-full rounded-card bg-primary px-4 py-2 text-sm font-semibold text-white disabled:opacity-40"
        >
          {busy ? 'Starting…' : 'Start Tracking'}
        </button>
      </div>
    </div>
  )
}
