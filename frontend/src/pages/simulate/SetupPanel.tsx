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

/** `off` stores no profile for the planned P2 bot. */
type BotChoice = 'off' | BotPreset

const BOT_OPTIONS: { value: BotChoice; label: string }[] = [
  { value: 'off', label: 'None' },
  { value: 'fast', label: 'Fast' },
  { value: 'balanced', label: 'Balanced' },
  { value: 'strong', label: 'Strong' },
]

// The first three algorithms solve the game exactly to the depth horizon.
// The last three sample, so they return an estimate.
const BOT_ALGORITHM_OPTIONS: { value: BotAlgorithm; label: string }[] = [
  { value: 'doubleOracle', label: 'Double Oracle (exact)' },
  { value: 'serializedBounds', label: 'Serialized Bounds (exact)' },
  { value: 'backwardInduction', label: 'Backward Induction (exact)' },
  { value: 'mcts', label: 'MCTS (sampled)' },
  { value: 'ismcts', label: 'ISMCTS (sampled belief)' },
  { value: 'mccfr', label: 'MCCFR (sampled belief)' },
]

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
  const [informationMode, setInformationMode] = useState<InformationMode>(
    saved?.informationMode ?? 'closedSheet',
  )
  const [botPreset, setBotPreset] = useState<BotChoice>(saved?.botPreset ?? 'off')
  const [botAlgorithm, setBotAlgorithm] = useState<BotAlgorithm>(
    saved?.botAlgorithm ?? 'doubleOracle',
  )

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
            onChange={(v) => setInformationMode(v as InformationMode)}
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
                options={BOT_ALGORITHM_OPTIONS}
                onChange={(v) => setBotAlgorithm(v as BotAlgorithm)}
              />
              <p className="mt-1 text-xs text-ink-muted">
                This stores limits for the planned P2 bot. P2 stays under hotseat control for now.
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
