import { useState } from 'react'
import { loadFormats, loadTeams } from '../../lib/storage'
import { useBattle } from '../../store/battleStore'

export default function SetupPanel() {
  const [teams] = useState(loadTeams)
  const [formats] = useState(loadFormats)
  const { createBattle, busy, error, clearError } = useBattle()

  const [formatId, setFormatId] = useState(formats[0]?.id ?? '')
  const [team1Id, setTeam1Id] = useState(teams[0]?.id ?? '')
  const [team2Id, setTeam2Id] = useState(teams[1]?.id ?? teams[0]?.id ?? '')

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

  const selectClass =
    'w-full rounded-card border border-subtle bg-surface px-3 py-2 text-sm outline-none focus:border-primary'

  return (
    <div className="mx-auto max-w-lg p-6">
      <div className="glass mt-12 rounded-card p-6 shadow-md">
        <h1 className="mb-1 text-xl font-semibold">New Battle</h1>
        <p className="mb-6 text-sm text-ink-muted">
          Pick a ruleset and two teams, then run the simulation hotseat-style.
        </p>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Ruleset</span>
          <select value={formatId} onChange={(e) => setFormatId(e.target.value)} className={selectClass}>
            {formats.map((f) => (
              <option key={f.id} value={f.id}>
                {f.name}
              </option>
            ))}
          </select>
        </label>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Player 1 team</span>
          <select value={team1Id} onChange={(e) => setTeam1Id(e.target.value)} className={selectClass}>
            {teams.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>

        <label className="mb-4 block text-sm">
          <span className="mb-1 block font-medium">Player 2 team</span>
          <select value={team2Id} onChange={(e) => setTeam2Id(e.target.value)} className={selectClass}>
            {teams.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>

        <label className="mb-6 block text-sm">
          <span className="mb-1 block font-medium">Information mode</span>
          <select className={selectClass} disabled>
            <option>Perfect Information</option>
          </select>
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
