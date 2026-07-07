import { useEffect, useState } from 'react'
import type { CommandOption } from '../../api/types'
import Sprite from '../../components/common/Sprite'
import { useBattle } from '../../store/battleStore'

/**
 * Floating command wizard. Walks the current player's active slots one at a
 * time; in doubles a multi-target move parks in `pendingAttack` and the Arena
 * handles target selection. Forced slots (Pass during selfSwitch/replacement)
 * are auto-committed so the user only ever clicks real choices.
 */
export default function ControlPanel() {
  const {
    view,
    commands,
    currentPlayer,
    draftCommands,
    pendingAttack,
    previewPicks,
    busy,
    pushSlotCommand,
    setPendingAttack,
    goBack,
    togglePreviewPick,
    submitPreview,
  } = useBattle()

  const currentSlot = draftCommands.length
  const [tera, setTera] = useState(false)
  const [mega, setMega] = useState(false)

  useEffect(() => {
    setTera(false)
    setMega(false)
  }, [currentPlayer, currentSlot])

  // Auto-commit forced slots. Reads fresh store state so a double-invoked
  // effect (StrictMode) sees the already-advanced draft and doesn't re-push.
  useEffect(() => {
    const s = useBattle.getState()
    const slot = s.commands?.slots[s.draftCommands.length]
    if (slot?.forced && slot.options.length > 0 && !s.busy) {
      s.pushSlotCommand(slot.options[0].command)
    }
  }, [commands, currentSlot, currentPlayer])

  if (!view || view.phase === 'gameOver') return null

  const panelClass =
    'glass absolute inset-x-0 bottom-3 z-20 mx-auto w-fit max-w-3xl rounded-card p-4 shadow-lg'

  const playerBadge = (
    <span
      className={`rounded px-2 py-0.5 text-[11px] font-bold text-white ${
        currentPlayer === 'p1' ? 'bg-primary' : 'bg-danger'
      }`}
    >
      {currentPlayer === 'p1' ? 'Player 1' : 'Player 2'}
    </span>
  )

  const backButton = (disabled: boolean) => (
    <button
      onClick={goBack}
      disabled={disabled}
      className="lift rounded-card border border-subtle px-3 py-1.5 text-xs font-medium text-ink-muted hover:text-ink disabled:opacity-40"
    >
      ← Back
    </button>
  )

  // ── Team preview: pick order, first activePerSide are leads ───────────────
  if (view.phase === 'teamPreview' && view.preview) {
    const preview = view.preview
    const mons = currentPlayer === 'p1' ? preview.p1Mons : preview.p2Mons
    const needed = Math.min(preview.broughtPerSide, mons.length)

    return (
      <div className={panelClass}>
        <div className="mb-3 flex items-center gap-2">
          {playerBadge}
          <span className="text-sm font-semibold">
            Pick {needed} Pokémon — first {preview.activePerSide}{' '}
            {preview.activePerSide === 1 ? 'leads' : 'lead'}
          </span>
        </div>

        <div className="grid grid-cols-3 gap-2">
          {mons.map((mon, i) => {
            const order = previewPicks.indexOf(i)
            const picked = order !== -1
            return (
              <button
                key={mon.monId}
                onClick={() => togglePreviewPick(i)}
                disabled={!picked && previewPicks.length >= needed}
                className={`lift relative flex w-28 flex-col items-center rounded-card p-2 ${
                  picked ? 'bg-subtle' : 'hover:bg-primary-soft'
                } disabled:opacity-40`}
              >
                {picked && (
                  <span className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-primary text-[11px] font-bold text-white">
                    {order + 1}
                  </span>
                )}
                {picked && order < preview.activePerSide && (
                  <span className="absolute left-1 top-1 rounded bg-success px-1 text-[9px] font-bold text-white">
                    Lead
                  </span>
                )}
                <Sprite species={mon.species} size={56} className={picked ? 'grayscale' : ''} />
                <span className="mt-1 max-w-full truncate text-xs font-medium">{mon.species}</span>
              </button>
            )
          })}
        </div>

        <div className="mt-3 flex items-center justify-between">
          {backButton(previewPicks.length === 0)}
          <button
            onClick={() => void submitPreview()}
            disabled={previewPicks.length !== needed || busy}
            className="lift rounded-card bg-primary px-4 py-1.5 text-xs font-semibold text-white disabled:opacity-40"
          >
            {currentPlayer === 'p1' ? 'Confirm — Player 2 next' : 'Start Battle'}
          </button>
        </div>
      </div>
    )
  }

  // ── Battle phases ──────────────────────────────────────────────────────────
  if (busy) {
    return (
      <div className={panelClass}>
        <span className="text-sm font-medium text-ink-muted">Resolving turn…</span>
      </div>
    )
  }
  if (!commands) {
    return (
      <div className={panelClass}>
        <span className="text-sm font-medium text-ink-muted">Loading commands…</span>
      </div>
    )
  }

  const slot = commands.slots[currentSlot]
  if (!slot || slot.forced) {
    // All slots committed (turn about to ship) or a forced slot the effect
    // above is about to auto-advance.
    return (
      <div className={panelClass}>
        <span className="text-sm font-medium text-ink-muted">…</span>
      </div>
    )
  }

  if (pendingAttack) {
    return (
      <div className={panelClass}>
        <div className="flex items-center gap-3">
          {playerBadge}
          <span className="text-sm font-medium">Click a highlighted target in the arena</span>
          {backButton(false)}
        </div>
      </div>
    )
  }

  const side = currentPlayer === 'p1' ? view.p1 : view.p2
  const activeMon = side?.active[slot.slotIndex]

  const attackOptions = slot.options.filter((o) => o.command.kind === 'attack')
  const struggleOptions = slot.options.filter((o) => o.command.kind === 'struggle')
  const switchOptions = slot.options.filter((o) => o.command.kind === 'switch')

  const moveSlots = new Map<number, CommandOption[]>()
  for (const option of attackOptions) {
    if (option.command.kind !== 'attack') continue
    const list = moveSlots.get(option.command.moveSlot) ?? []
    list.push(option)
    moveSlots.set(option.command.moveSlot, list)
  }

  const hasTera = attackOptions.some((o) => o.command.kind === 'attack' && o.command.terastallize)
  const hasMega = attackOptions.some((o) => o.command.kind === 'attack' && o.command.megaEvolve)

  const optionsFor = (moveSlot: number) =>
    (moveSlots.get(moveSlot) ?? []).filter(
      (o) =>
        o.command.kind === 'attack' &&
        !!o.command.terastallize === tera &&
        !!o.command.megaEvolve === mega,
    )

  const commit = (options: CommandOption[], moveSlot: number) => {
    if (options.length === 1) {
      pushSlotCommand(options[0].command)
    } else if (options.length > 1) {
      setPendingAttack({ moveSlot, terastallize: tera, megaEvolve: mega, targets: options })
    }
  }

  const phaseHint =
    commands.phase === 'selfSwitch'
      ? 'Choose a Pokémon to switch in'
      : commands.phase === 'replacement'
        ? 'Choose a replacement'
        : null

  return (
    <div className={panelClass}>
      <div className="mb-2 flex items-center gap-2">
        {playerBadge}
        <span className="text-sm font-semibold">
          {phaseHint ?? (activeMon ? activeMon.species : `Slot ${slot.slotIndex + 1}`)}
        </span>
        {commands.slots.length > 1 && !phaseHint && (
          <span className="text-[11px] text-ink-muted">
            slot {slot.slotIndex + 1} of {commands.slots.length}
          </span>
        )}
        <div className="ml-auto flex gap-1.5">
          {hasTera && (
            <button
              onClick={() => setTera((t) => !t)}
              className={`lift rounded-card px-2 py-1 text-[11px] font-semibold ${
                tera ? 'bg-primary text-white' : 'border border-subtle text-ink-muted'
              }`}
            >
              ✦ Tera{activeMon ? ` (${activeMon.teraType})` : ''}
            </button>
          )}
          {hasMega && (
            <button
              onClick={() => setMega((m) => !m)}
              className={`lift rounded-card px-2 py-1 text-[11px] font-semibold ${
                mega ? 'bg-primary text-white' : 'border border-subtle text-ink-muted'
              }`}
            >
              Mega
            </button>
          )}
        </div>
      </div>

      {moveSlots.size > 0 && (
        <div className="grid grid-cols-2 gap-1.5">
          {[...moveSlots.keys()].sort((a, b) => a - b).map((moveSlot) => {
            const group = moveSlots.get(moveSlot) ?? []
            const available = optionsFor(moveSlot)
            const moveView = activeMon?.moves[moveSlot]
            return (
              <button
                key={moveSlot}
                onClick={() => commit(available, moveSlot)}
                disabled={available.length === 0}
                className="lift rounded-card bg-primary-soft px-3 py-2 text-left text-sm font-medium text-primary hover:bg-primary hover:text-white disabled:opacity-40"
              >
                {group[0]?.label ?? `Move ${moveSlot + 1}`}
                {moveView && (
                  <span className="ml-2 text-[10px] opacity-70">
                    {moveView.pp}/{moveView.maxPp}
                  </span>
                )}
              </button>
            )
          })}
        </div>
      )}

      {struggleOptions.length > 0 && (
        <button
          onClick={() => commit(struggleOptions, -1)}
          className="lift mt-1.5 w-full rounded-card bg-danger/10 px-3 py-2 text-sm font-medium text-danger hover:bg-danger hover:text-white"
        >
          Struggle
        </button>
      )}

      {switchOptions.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1.5">
          {switchOptions.map((option, i) => (
            <button
              key={i}
              onClick={() => pushSlotCommand(option.command)}
              className="lift flex items-center gap-1.5 rounded-card border border-subtle px-2 py-1 text-xs font-medium hover:bg-primary-soft"
            >
              {option.label && <Sprite species={option.label} size={28} />}
              {option.label ?? 'Switch'}
            </button>
          ))}
        </div>
      )}

      <div className="mt-2">{backButton(draftCommands.length === 0)}</div>
    </div>
  )
}
