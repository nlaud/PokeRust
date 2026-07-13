// EventNode → battle-log lines. Events carry slot references, not names, and
// the Pokémon in a slot changes over the battle — so the log is rendered as a
// single chronological walk that maintains a slot→species map, updated by the
// switch events as they pass.

import type { EventNode, FieldSlot, ObservedHp, PlayerId, TurnLogEntry } from '../api/types'

export type Tone = 'default' | 'muted' | 'success' | 'danger' | 'primary' | 'warning'

export interface LogLine {
  depth: number
  text: string
  tone: Tone
}

export interface RenderedTurn {
  label: string
  lines: LogLine[]
}

const STATUS_NAMES: Record<string, string> = {
  BRN: 'burned',
  PSN: 'poisoned',
  TOX: 'badly poisoned',
  PAR: 'paralyzed',
  SLP: 'put to sleep',
  FRZ: 'frozen',
}

function slotKey(slot: FieldSlot): string {
  return `${slot.player}-${slot.slotIndex}`
}

function playerLabel(player: PlayerId): string {
  return player === 'p1' ? 'P1' : 'P2'
}

function hpText(hp: ObservedHp): string {
  if (hp.exact !== undefined) return `${hp.exact} HP`
  if (hp.percent !== undefined) return `${hp.percent}%`
  return '?'
}

class NameResolver {
  private names = new Map<string, string>()

  learn(slot: FieldSlot, species: string) {
    this.names.set(slotKey(slot), species)
  }

  name(slot: FieldSlot): string {
    const species = this.names.get(slotKey(slot))
    const owner = playerLabel(slot.player)
    return species ? `${owner}'s ${species}` : `${owner} slot ${slot.slotIndex + 1}`
  }
}

function renderEvent(event: EventNode, depth: number, resolver: NameResolver, out: LogLine[]) {
  const line = (text: string, tone: Tone = 'default') => out.push({ depth, text, tone })

  switch (event.type) {
    case 'moveUsed':
      line(`${resolver.name(event.user)} used ${event.move}!`)
      break
    case 'switch':
      resolver.learn(event.switch.slot, event.switch.species)
      line(
        `${playerLabel(event.switch.slot.player)} sent out ${event.switch.species} (${hpText(event.switch.hp)})`,
      )
      break
    case 'simultaneousSwitch':
      for (const sw of event.switches) {
        resolver.learn(sw.slot, sw.species)
        line(`${playerLabel(sw.slot.player)} sent out ${sw.species} (${hpText(sw.hp)})`)
      }
      break
    case 'endOfTurn':
      if (event.reactions.length > 0) line('End of turn', 'muted')
      break
    case 'faint':
      line(`${resolver.name(event.slot)} fainted!`, 'danger')
      break
    case 'megaEvolution':
      line(`${resolver.name(event.slot)} Mega Evolved into ${event.into}!`, 'primary')
      resolver.learn(event.slot, event.into)
      break
    case 'terastallization':
      line(`${resolver.name(event.slot)} Terastallized into the ${event.teraType} type!`, 'primary')
      break
    case 'formeChange':
      line(`${resolver.name(event.slot)} changed forme into ${event.into}!`, 'primary')
      resolver.learn(event.slot, event.into)
      break
    case 'typeChanged':
      line(`${resolver.name(event.slot)} became ${event.newTypes.join('/')}-type!`, 'primary')
      break
    case 'cant':
      line(`${resolver.name(event.slot)} can't act (${event.reason})`, 'warning')
      break
    case 'chargingMove':
      line(`${resolver.name(event.user)} is charging ${event.move}…`, 'muted')
      break
    case 'mustRecharge':
      line(`${resolver.name(event.slot)} must recharge!`, 'muted')
      break
    case 'singleMoveOrTurn': {
      // The protection family reads as the in-game line, not "used X!"
      // (the MoveUsed event above already covers the "used" phrasing).
      const PROTECTS = ['Protect', 'Detect', 'Spiky Shield', 'Baneful Bunker', "King's Shield"]
      if (PROTECTS.includes(event.move)) {
        line(`${resolver.name(event.slot)} protected itself!`, 'primary')
      } else if (event.move === 'Endure') {
        line(`${resolver.name(event.slot)} braced itself!`, 'primary')
      } else {
        line(`${resolver.name(event.slot)} used ${event.move}!`)
      }
      break
    }
    case 'damageDealt':
      line(`${resolver.name(event.target)} took damage (now ${hpText(event.newHp)})`, 'danger')
      break
    case 'healed':
      line(`${resolver.name(event.target)} recovered HP (now ${hpText(event.newHp)})`, 'success')
      break
    case 'setHp':
      line(`${resolver.name(event.target)}'s HP was set to ${hpText(event.newHp)}`, 'warning')
      break
    case 'crit':
      line(`A critical hit on ${resolver.name(event.target)}!`, 'warning')
      break
    case 'immune':
      line(`It doesn't affect ${resolver.name(event.target)}…`, 'muted')
      break
    case 'missed':
      line(`The attack missed ${resolver.name(event.target)}!`, 'muted')
      break
    case 'moveFailed':
      line(`${resolver.name(event.slot)}'s move failed!`, 'muted')
      break
    case 'blocked':
      line(`${resolver.name(event.target)} blocked the attack!`, 'muted')
      break
    case 'hitCount':
      line(`Hit ${event.hits} time(s)!`, 'muted')
      break
    case 'statusInflicted':
      line(
        `${resolver.name(event.target)} was ${STATUS_NAMES[event.status.code] ?? event.status.code}!`,
        'danger',
      )
      break
    case 'statusCured':
      line(
        `${resolver.name(event.target)} was cured of ${STATUS_NAMES[event.status.code] ?? event.status.code}!`,
        'success',
      )
      break
    case 'teamStatusCured':
      line(`${playerLabel(event.side)}'s team was cured of status conditions!`, 'success')
      break
    case 'boostChanged': {
      const dir = event.stages > 0 ? 'rose' : 'fell'
      const amount = Math.abs(event.stages)
      line(
        `${resolver.name(event.target)}'s ${event.stat} ${dir}${amount > 1 ? ` by ${amount}` : ''}! (${event.stages > 0 ? '+' : ''}${event.stages})`,
        event.stages > 0 ? 'success' : 'danger',
      )
      break
    }
    case 'boostsCleared':
      line(`${resolver.name(event.target)}'s stat changes were removed!`, 'muted')
      break
    case 'boostsInverted':
      line(`${resolver.name(event.target)}'s stat changes were inverted!`, 'warning')
      break
    case 'boostsSwapped':
      line(
        `${resolver.name(event.source)} swapped stat changes with ${resolver.name(event.target)}!`,
        'primary',
      )
      break
    case 'boostsCopied':
      line(
        `${resolver.name(event.target)} copied ${resolver.name(event.source)}'s stat changes!`,
        'primary',
      )
      break
    case 'weatherChanged':
      line(event.weather ? `The weather became ${event.weather}!` : 'The weather cleared.', 'primary')
      break
    case 'terrainChanged':
      line(event.terrain ? `${event.terrain} covered the field!` : 'The terrain faded.', 'primary')
      break
    case 'pseudoWeatherStart':
      line(`${event.effect} took effect!`, 'primary')
      break
    case 'pseudoWeatherEnd':
      line(`${event.effect} ended.`, 'muted')
      break
    case 'sideConditionStart':
      line(`${event.condition} was set on ${playerLabel(event.side)}'s side!`, 'primary')
      break
    case 'sideConditionEnd':
      line(`${event.condition} ended on ${playerLabel(event.side)}'s side.`, 'muted')
      break
    case 'slotConditionStart':
      line(`${event.condition} was set on ${resolver.name(event.slot)}'s position!`, 'primary')
      break
    case 'slotConditionEnd':
      line(`${event.condition} resolved at ${resolver.name(event.slot)}'s position.`, 'muted')
      break
    case 'volatileStart':
      line(`${resolver.name(event.target)} gained ${event.volatile}!`, 'warning')
      break
    case 'volatileEnd':
      line(`${resolver.name(event.target)}'s ${event.volatile} ended.`, 'muted')
      break
    case 'perishCount':
      line(`${resolver.name(event.target)}'s perish count: ${event.turnsLeft}`, 'danger')
      break
    case 'itemRevealed':
      line(`${resolver.name(event.slot)} is holding ${event.item}!`, 'primary')
      break
    case 'itemGained':
      line(`${resolver.name(event.slot)} obtained ${event.item}!`, 'primary')
      break
    case 'itemLost':
      line(
        event.consumed
          ? `${resolver.name(event.slot)} used its ${event.item}!`
          : `${resolver.name(event.slot)} lost its ${event.item}!`,
        'warning',
      )
      break
    case 'abilityRevealed':
      line(`${resolver.name(event.slot)}'s ${event.ability}!`, 'primary')
      break
    case 'anticipationShudder':
      line(`${resolver.name(event.slot)} shuddered!`, 'warning')
      break
    case 'illusionEnded':
      line(`${resolver.name(event.slot)}'s Illusion wore off — it was ${event.actualSpecies}!`, 'warning')
      resolver.learn(event.slot, event.actualSpecies)
      break
    case 'transformed':
      line(`${resolver.name(event.slot)} transformed into ${event.intoSpecies}!`, 'primary')
      break
    default: {
      // Unknown future event types must never crash the log.
      const unknown = event as { type: string }
      line(`${unknown.type} event`, 'muted')
    }
  }

  // endOfTurn children render at the same visual depth as their header line.
  const childDepth = event.type === 'endOfTurn' && event.reactions.length === 0 ? depth : depth + 1
  for (const reaction of event.reactions) {
    renderEvent(reaction, childDepth, resolver, out)
  }
}

/** Render the full battle log in one chronological pass.
 *
 * Consecutive entries with the same label (e.g. a U-turn self-switch that
 * arrives as a separate server round-trip at the same turn number) are
 * coalesced into a single RenderedTurn so the sidebar only draws one divider. */
export function renderLog(entries: TurnLogEntry[]): RenderedTurn[] {
  const resolver = new NameResolver()
  const out: RenderedTurn[] = []
  for (const entry of entries) {
    const lines: LogLine[] = []
    for (const event of entry.events) {
      renderEvent(event, 0, resolver, lines)
    }
    const prev = out[out.length - 1]
    if (prev && prev.label === entry.label) {
      // Same turn — append to the existing block instead of opening a new one.
      prev.lines.push(...lines)
    } else {
      out.push({ label: entry.label, lines })
    }
  }
  return out
}
