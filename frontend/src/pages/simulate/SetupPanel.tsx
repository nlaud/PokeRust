import { useEffect, useState } from 'react'
import Select from '../../components/common/Select'
import { loadBattleSetup, loadFormats, loadTeams, saveBattleSetup } from '../../lib/storage'
import { useBattle } from '../../store/battleStore'

export default function SetupPanel() {
  const [teams] = useState(loadTeams)
  const [formats] = useState(loadFormats)
  const { createBattle, busy, error, clearError } = useBattle()

  // Restore the last-used configuration (ignoring ids that no longer exist);
  // otherwise Doubles is the default format when present (covers stored
  // format lists that still have Singles first).
  const saved = loadBattleSetup()
  const savedFormat = formats.find((f) => f.id === saved?.formatId)
  const savedTeam1 = teams.find((t) => t.id === saved?.team1Id)
  const savedTeam2 = teams.find((t) => t.id === saved?.team2Id)
  const defaultFormat =
    savedFormat ?? formats.find((f) => f.id === 'champions-s2-doubles') ?? formats[0]
  const [formatId, setFormatId] = useState(defaultFormat?.id ?? '')
  const [team1Id, setTeam1Id] = useState(savedTeam1?.id ?? teams[0]?.id ?? '')
  const [team2Id, setTeam2Id] = useState(savedTeam2?.id ?? teams[1]?.id ?? teams[0]?.id ?? '')

  useEffect(() => {
    saveBattleSetup({ formatId, team1Id, team2Id })
  }, [formatId, team1Id, team2Id])

  const format = formats.find((f) => f.id === formatId)
  const team1 = teams.find((t) => t.id === team1Id)
  const team2 = teams.find((t) => t.id === team2Id)
  const ready = format && team1 && team2

  const start = () => {
    if (!ready) return
    clearError()
    // The server resolves turns in sample mode (one weighted trajectory, not
    // the full outcome tree), so full damage-roll granularity is cheap in any
    // format.
    void createBattle({
      p1Team: team1.sheet,
      p2Team: team2.sheet,
      activePerSide: format.activePokemon,
      broughtPerSide: format.broughtPokemon,
      damageRolls: 16,
    })
  }

  const formatOptions = formats.map((f) => ({ value: f.id, label: f.name }))
  const teamOptions = teams.map((t) => ({ value: t.id, label: t.name }))

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

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Player 1 team</span>
          <Select value={team1Id} options={teamOptions} onChange={setTeam1Id} />
        </label>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Player 2 team</span>
          <Select value={team2Id} options={teamOptions} onChange={setTeam2Id} />
        </label>

        <label className="mb-6 block text-sm">
          <span className="mb-1 block font-medium">Information mode</span>
          <Select
            value="perfect"
            options={[{ value: 'perfect', label: 'Perfect Information' }]}
            onChange={() => {}}
            disabled
          />
        </label>

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
          {busy ? 'Starting…' : 'Start Battle'}
        </button>
      </div>
    </div>
  )
}
