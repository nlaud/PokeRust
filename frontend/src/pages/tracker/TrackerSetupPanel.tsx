import { useEffect, useState } from 'react'
import { listSpecies } from '../../api/client'
import Select from '../../components/common/Select'
import { CATALOG } from '../../lib/items'
import { favoritesFirst, loadFormats, loadTeams, type StoredFormat } from '../../lib/storage'
import { useTracker } from '../../store/trackerStore'
import SpeciesPicker from './SpeciesPicker'

/** Returns the permitted items for a format. */
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

/** Describes the opponent input for each information mode. */
const OPPONENT_HINT: Record<TrackerInfoMode, string> = {
  closedSheet:
    "Just species — pick or paste the opponent's roster; item/ability/moves/nature stay hidden either way.",
  openSheet:
    'A full teamsheet: species @ item, ability, and moves are visible under Open Team Sheet — nature and EVs/IVs stay hidden.',
  openSheetNatures:
    'A full teamsheet: species @ item, ability, moves, and nature are all visible under Open Team Sheet + Natures — EVs/IVs stay hidden.',
}
// Every placeholder species must have a Champions learnset.
// The server rejects a roster species that the learnset dex does not hold.
const OPPONENT_PLACEHOLDER: Record<TrackerInfoMode, string> = {
  closedSheet: 'Garchomp, Incineroar, Toxapex, Gholdengo, Kingambit, Dragonite',
  openSheet: 'Garchomp @ Rocky Helmet\nAbility: Rough Skin\n- Earthquake\n- Stealth Rock\n...',
  openSheetNatures:
    'Garchomp @ Rocky Helmet\nAbility: Rough Skin\nJolly Nature\n- Earthquake\n- Stealth Rock\n...',
}

/**
 * Starts a tracker session with a format, information mode, and opponent data.
 * The opponent data can be a teamsheet or species list.
 * A later `leads` event sends out both teams.
 */
export default function TrackerSetupPanel() {
  const [teams] = useState(loadTeams)
  const [formats] = useState(loadFormats)
  const { create, busy, error, clearError } = useTracker()

  // Select the first favorite team.
  // Use storage order when no team is a favorite.
  const sortedTeams = favoritesFirst(teams)
  const defaultFormat = formats.find((f) => f.id === 'champions-s2-doubles') ?? formats[0]
  const [formatId, setFormatId] = useState(defaultFormat?.id ?? '')
  const [teamId, setTeamId] = useState(sortedTeams[0]?.id ?? '')
  const [informationMode, setInformationMode] = useState<TrackerInfoMode>('closedSheet')
  const [opponent, setOpponent] = useState('')
  // Use the species picker for a closed sheet.
  // Keep picker data separate from the full opponent sheet.
  const [opponentSpecies, setOpponentSpecies] = useState<string[]>([])
  const [speciesCatalog, setSpeciesCatalog] = useState<string[]>([])
  const [speciesLoading, setSpeciesLoading] = useState(true)

  // Load the static species list once for the page.
  useEffect(() => {
    let cancelled = false
    listSpecies()
      .then((res) => {
        if (!cancelled) setSpeciesCatalog(res.species)
      })
      .catch(() => {
        // Keep the page available after a species-list failure.
        // The user can select another information mode.
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
  // Require at least `broughtPerSide` valid Pokémon.
  // Do not require a fixed roster of six.
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
      // `normalize_opponent_text` separates the comma-delimited species.
      opponent: usesPicker ? opponentSpecies.join(', ') : opponent,
      activePerSide: format.activePokemon,
      broughtPerSide: format.broughtPokemon,
      forceMaxIvs: format.forceMaxIvs,
      teraEnabled: format.teraEnabled,
      megaEnabled: format.megaEnabled,
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
