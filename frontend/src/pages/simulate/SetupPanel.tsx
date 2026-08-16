import { useEffect, useState } from 'react'
import Select from '../../components/common/Select'
import type { BotAlgorithm, BotPreset, InformationMode } from '../../api/types'
import { CATALOG } from '../../lib/items'
import { favoritesFirst, loadBattleSetup, loadFormats, loadTeams, saveBattleSetup, type StoredFormat } from '../../lib/storage'
import { useBattle } from '../../store/battleStore'

/** Returns all catalog items that the format permits. */
function legalItemsFor(format: StoredFormat): string[] {
  const banned = new Set(format.bannedItems)
  return CATALOG.filter((item) => !banned.has(item.name)).map((item) => item.name)
}

const INFO_MODE_OPTIONS: { value: InformationMode; label: string }[] = [
  { value: 'closedSheet', label: 'Closed Team Sheet' },
  { value: 'perfect', label: 'Perfect Information' },
  { value: 'openSheet', label: 'Open Team Sheet' },
  { value: 'openSheetNatures', label: 'Open Team Sheet + Natures' },
]

/** `off` creates a hotseat battle. */
type BotChoice = 'off' | BotPreset

const BOT_OPTIONS: { value: BotChoice; label: string; hint: string }[] = [
  { value: 'off', label: 'None', hint: 'Both players use hotseat input.' },
  { value: 'fast', label: 'Fast', hint: 'The shortest search. P2 answers in about a second.' },
  {
    value: 'balanced',
    label: 'Balanced',
    hint: 'A longer search than Fast. P2 takes a few seconds each turn.',
  },
  {
    value: 'strong',
    label: 'Strong',
    hint: 'The longest search. P2 can take tens of seconds each turn.',
  },
]

// The first three algorithms solve the game exactly to the depth horizon.
// The last three sample, so they return an estimate.
const BOT_ALGORITHM_OPTIONS: { value: BotAlgorithm; label: string; hint: string }[] = [
  {
    value: 'doubleOracle',
    label: 'Double Oracle (exact)',
    hint: 'Exact: it solves every turn to the depth horizon and returns the true mixed strategy of that horizon. It reads the true position, so it sees through the fog of war.',
  },
  {
    value: 'serializedBounds',
    label: 'Serialized Bounds (exact)',
    hint: 'Exact: the same answer as Double Oracle through alpha-beta bounds. It reads the true position, so it sees through the fog of war.',
  },
  {
    value: 'backwardInduction',
    label: 'Backward Induction (exact)',
    hint: 'Exact: it builds the whole payoff matrix of every turn. The slowest exact algorithm. It reads the true position, so it sees through the fog of war.',
  },
  {
    value: 'mcts',
    label: 'MCTS (sampled)',
    hint: 'Sampled: it plays random lines and keeps the best. The answer is an estimate. It reads the true position, so it sees through the fog of war.',
  },
  {
    value: 'ismcts',
    label: 'ISMCTS (sampled belief)',
    hint: 'Sampled: it draws a possible version of your team from the belief, then searches. The answer is an estimate, and it respects the fog of war.',
  },
  {
    value: 'mccfr',
    label: 'MCCFR (sampled belief)',
    hint: 'Sampled: it learns a mixed strategy from repeated self-play over the belief. The answer is an estimate, and it respects the fog of war.',
  },
]

// An algorithm reads the true position, or it reads a belief.
// A session hides the data of the other player, or it hides nothing.
// A pair plays only when the two answers agree.
// The picker disables every other pair, and `bot_algorithm_fits_mode` in
// `poke_rust/src/bin/server/routes.rs` rejects one that still arrives.
// `strategy_respects_fog` and `belief_search_inputs` in
// `poke_rust/src/bin/server/analysis.rs` hold the same rule.
// A new algorithm needs one entry here and one arm there.
const BELIEF_ALGORITHMS: BotAlgorithm[] = ['ismcts', 'mccfr']

/** True when this information mode hides data from P2. */
function hidesData(mode: InformationMode): boolean {
  return mode !== 'perfect'
}

/** True when this algorithm can control P2 under this information mode. */
function canPlay(algorithm: BotAlgorithm, mode: InformationMode): boolean {
  return BELIEF_ALGORITHMS.includes(algorithm) === hidesData(mode)
}

/** The algorithm that plays under this information mode. */
function defaultAlgorithmFor(mode: InformationMode): BotAlgorithm {
  return hidesData(mode) ? 'ismcts' : 'doubleOracle'
}

/**
 * Names the reason that this mode limits the algorithm list.
 *
 * The picker shows this line at all times. It explains the disabled entries,
 * which the list keeps so that the user reads the whole set.
 */
function algorithmLimitNote(mode: InformationMode): string {
  return hidesData(mode)
    ? 'This information mode hides Player 1 data, so only a belief search can control P2. The other algorithms read the true position.'
    : 'Perfect Information holds no belief, so only a search of the true position can control P2. Pick another information mode for a belief search.'
}

type TeamSource = 'saved' | 'meta'

const TEAM_SOURCE_OPTIONS: { value: TeamSource; label: string }[] = [
  { value: 'saved', label: 'Saved team' },
  { value: 'meta', label: 'Generate from meta' },
]

export default function SetupPanel() {
  const [teams] = useState(loadTeams)
  const [formats] = useState(loadFormats)
  const { createBattle, busy, error, clearError } = useBattle()

  // Restore valid values from the last configuration.
  // Otherwise, select Doubles and the first favorite teams.
  // Use storage order when no team is a favorite.
  const saved = loadBattleSetup()
  const sortedTeams = favoritesFirst(teams)
  const savedFormat = formats.find((f) => f.id === saved?.formatId)
  const savedTeam1 = teams.find((t) => t.id === saved?.team1Id)
  const savedTeam2 = teams.find((t) => t.id === saved?.team2Id)
  const defaultFormat =
    savedFormat ?? formats.find((f) => f.id === 'champions-s2-doubles') ?? formats[0]
  const [formatId, setFormatId] = useState(defaultFormat?.id ?? '')
  const [team1Id, setTeam1Id] = useState(savedTeam1?.id ?? sortedTeams[0]?.id ?? '')
  const [team2Id, setTeam2Id] = useState(
    savedTeam2?.id ?? sortedTeams[1]?.id ?? sortedTeams[0]?.id ?? '',
  )
  const [team1Source, setTeam1Source] = useState<TeamSource>(saved?.team1Source ?? 'saved')
  const [team2Source, setTeam2Source] = useState<TeamSource>(saved?.team2Source ?? 'saved')
  const defaultInformationMode = saved?.informationMode ?? 'closedSheet'
  const [informationMode, setInformationMode] = useState<InformationMode>(defaultInformationMode)
  const [botPreset, setBotPreset] = useState<BotChoice>(saved?.botPreset ?? 'off')
  const savedAlgorithm = saved?.botAlgorithm
  // A stored algorithm that cannot play under the stored mode creates a bot
  // that never answers, so the load path replaces it. A setup from an older
  // picker can hold such a pair.
  const [botAlgorithm, setBotAlgorithm] = useState<BotAlgorithm>(
    savedAlgorithm && canPlay(savedAlgorithm, defaultInformationMode)
      ? savedAlgorithm
      : defaultAlgorithmFor(defaultInformationMode),
  )

  // A mode change replaces an algorithm that cannot play in the new mode.
  const changeInformationMode = (mode: InformationMode) => {
    setInformationMode(mode)
    if (!canPlay(botAlgorithm, mode)) setBotAlgorithm(defaultAlgorithmFor(mode))
  }

  useEffect(() => {
    saveBattleSetup({
      formatId,
      team1Id,
      team2Id,
      team1Source,
      team2Source,
      informationMode,
      botPreset,
      botAlgorithm,
    })
  }, [
    formatId,
    team1Id,
    team2Id,
    team1Source,
    team2Source,
    informationMode,
    botPreset,
    botAlgorithm,
  ])

  const format = formats.find((f) => f.id === formatId)
  const team1 = teams.find((t) => t.id === team1Id)
  const team2 = teams.find((t) => t.id === team2Id)
  // A meta side generates its team on the server.
  // Only a saved side requires a selected team.
  const ready = format && (team1Source === 'meta' || team1) && (team2Source === 'meta' || team2)

  const start = () => {
    if (!ready) return
    clearError()
    // Sample mode resolves one weighted path.
    // Full damage-roll detail is therefore practical in each format.
    void createBattle({
      p1Team: team1Source === 'saved' ? (team1?.sheet ?? '') : '',
      p2Team: team2Source === 'saved' ? (team2?.sheet ?? '') : '',
      p1TeamMode: team1Source === 'meta' ? 'meta' : 'sheet',
      p2TeamMode: team2Source === 'meta' ? 'meta' : 'sheet',
      activePerSide: format.activePokemon,
      broughtPerSide: format.broughtPokemon,
      // A generated side gets the full roster of the format.
      // Team preview then picks the brought Pokemon out of it.
      totalPerSide: format.totalPokemon,
      forceMaxIvs: format.forceMaxIvs,
      teraEnabled: format.teraEnabled,
      megaEnabled: format.megaEnabled,
      damageRolls: 16,
      informationMode,
      legalItems: legalItemsFor(format),
      // The server resolves the preset and returns every limit it applied.
      botP2: botPreset === 'off' ? undefined : { algorithm: botAlgorithm, preset: botPreset },
    })
  }

  const algorithmHint = BOT_ALGORITHM_OPTIONS.find((o) => o.value === botAlgorithm)?.hint ?? ''
  // The list keeps every algorithm and disables each one that cannot play. The
  // user then reads the whole set and cannot select a pair that never answers.
  const algorithmOptions = BOT_ALGORITHM_OPTIONS.map((option) => ({
    ...option,
    disabled: !canPlay(option.value, informationMode),
  }))
  const formatOptions = favoritesFirst(formats).map((f) => ({ value: f.id, label: f.name }))
  const teamOptions = sortedTeams.map((t) => ({ value: t.id, label: t.name }))

  return (
    <div className="mx-auto max-w-lg p-6">
      <div className="glass mt-12 rounded-card p-6 shadow-md">
        <h1 className="mb-1 text-xl font-semibold">New Battle</h1>
        <p className="mb-6 text-sm text-ink-muted">
          Pick a ruleset and two teams, then run the simulation hotseat-style.
        </p>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Ruleset</span>
          <Select value={formatId} options={formatOptions} onChange={setFormatId} />
        </label>

        <div className="mb-4">
          <span className="mb-1 block text-sm font-medium">Player 1 team</span>
          <div className="mb-2">
            <Select
              value={team1Source}
              options={TEAM_SOURCE_OPTIONS}
              onChange={(v) => setTeam1Source(v as TeamSource)}
            />
          </div>
          {team1Source === 'saved' && (
            <Select value={team1Id} options={teamOptions} onChange={setTeam1Id} />
          )}
        </div>

        <div className="mb-4">
          <span className="mb-1 block text-sm font-medium">Player 2 team</span>
          <div className="mb-2">
            <Select
              value={team2Source}
              options={TEAM_SOURCE_OPTIONS}
              onChange={(v) => setTeam2Source(v as TeamSource)}
            />
          </div>
          {team2Source === 'saved' && (
            <Select value={team2Id} options={teamOptions} onChange={setTeam2Id} />
          )}
        </div>

        <label className="mb-6 block text-sm">
          <span className="mb-1 block font-medium">Information mode</span>
          <Select
            value={informationMode}
            options={INFO_MODE_OPTIONS}
            onChange={(v) => changeInformationMode(v as InformationMode)}
          />
        </label>

        <div className="mb-6" data-testid="bot-picker">
          <span className="mb-1 block text-sm font-medium">P2 solver profile</span>
          <div className="mb-2">
            <Select
              value={botPreset}
              options={BOT_OPTIONS}
              onChange={(v) => setBotPreset(v as BotChoice)}
            />
          </div>
          {botPreset !== 'off' && (
            <>
              <Select
                value={botAlgorithm}
                options={algorithmOptions}
                onChange={(v) => setBotAlgorithm(v as BotAlgorithm)}
              />
              <p className="mt-1 text-xs text-ink-muted" data-testid="bot-algorithm-hint">
                {algorithmHint}
              </p>
              <p className="mt-1 text-xs text-ink-muted" data-testid="bot-algorithm-limit">
                {algorithmLimitNote(informationMode)}
              </p>
              <p className="mt-1 text-xs text-ink-muted">
                P2 uses this solver profile after Player 1 locks a command.
              </p>
            </>
          )}
        </div>

        {teams.length === 0 && (team1Source === 'saved' || team2Source === 'saved') && (
          <p className="mb-4 text-sm text-warning">
            No teams yet — create one on the Teams page, or switch a side to
            &ldquo;Generate from meta&rdquo;.
          </p>
        )}
        {error && <p className="mb-4 text-sm text-danger">{error}</p>}

        <button
          onClick={start}
          disabled={!ready || busy}
          className="lift w-full rounded-card bg-primary px-4 py-2 text-sm font-semibold text-white disabled:opacity-40"
        >
          {busy ? 'Starting…' : 'Start Battle'}
        </button>
      </div>
    </div>
  )
}
