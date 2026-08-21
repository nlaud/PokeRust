import { useEffect, useState } from 'react'
import Select from '../../components/common/Select'
import { solverHint, solverLabel, solverProfile } from '../../components/solver/solverSettings'
import type { InformationMode } from '../../api/types'
import { CATALOG } from '../../lib/items'
import { favoritesFirst, loadBattleSetup, loadFormats, loadTeams, saveBattleSetup, type StoredFormat } from '../../lib/storage'
import { useBattle } from '../../store/battleStore'
import { useSettings } from '../../store/settingsStore'

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

const BOT_OPTIONS = [
  { value: 'off', label: 'None', hint: 'Both players use hotseat input.' },
  { value: 'on', label: 'Solver', hint: 'The solver controls Player 2.' },
]

/** True when this information mode hides data from P2. */
function hidesData(mode: InformationMode): boolean {
  return mode !== 'perfect'
}

/**
 * Names the search that P2 uses under this information mode, and why.
 *
 * The mode picks the category, and the Settings sidebar picks the search inside
 * that category. The pair can therefore never disagree, so this panel offers no
 * algorithm list of its own and needs no repair path for a stored pair.
 *
 * `bot_algorithm_fits_mode` in `poke_rust/src/bin/server/routes.rs` holds the
 * same rule and returns 422 for a pair that still arrives.
 */
function algorithmModeNote(mode: InformationMode): string {
  return hidesData(mode)
    ? 'This information mode hides Player 1 data, so only a belief search can control P2. P2 uses your imperfect-information search from Settings.'
    : 'Perfect Information holds no belief, so only a search of the true position can control P2. P2 uses your perfect-information search from Settings.'
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
  const { solverPreset, solverSettings, imperfectSolver, perfectSolver } = useSettings()

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
  const [botEnabled, setBotEnabled] = useState(saved?.botEnabled ?? false)
  // The mode selects the category and Settings selects the search inside it, so
  // this panel stores no algorithm and a mode change repairs nothing.
  const botAlgorithm = hidesData(informationMode) ? imperfectSolver : perfectSolver

  useEffect(() => {
    saveBattleSetup({
      formatId,
      team1Id,
      team2Id,
      team1Source,
      team2Source,
      informationMode,
      botEnabled,
    })
  }, [formatId, team1Id, team2Id, team1Source, team2Source, informationMode, botEnabled])

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
      considerCrit: true,
      informationMode,
      legalItems: legalItemsFor(format),
      botP2:
        !botEnabled
          ? undefined
          : solverProfile(botAlgorithm, solverSettings, solverPreset),
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
              value={botEnabled ? 'on' : 'off'}
              options={BOT_OPTIONS}
              onChange={(value) => setBotEnabled(value === 'on')}
            />
          </div>
          {botEnabled && (
            <>
              <p className="text-sm font-semibold" data-testid="bot-algorithm-name">
                {solverLabel(botAlgorithm)}
              </p>
              <p className="mt-1 text-xs text-ink-muted" data-testid="bot-algorithm-hint">
                {solverHint(botAlgorithm)}
              </p>
              <p className="mt-1 text-xs text-ink-muted" data-testid="bot-algorithm-limit">
                {algorithmModeNote(informationMode)}
              </p>
              <p className="mt-1 text-xs text-ink-muted">
                P2 uses the {solverPreset === 'competitive' ? 'high' : solverPreset} limits from Settings. Its live strategy stays visible.
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
