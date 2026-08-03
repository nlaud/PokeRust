//! Defines events that reveal battle information to a player.
//!
//! The model follows the Showdown SIM-PROTOCOL.
//! [`InformationEvent::reactions`] nests each reaction below its cause.
//!
//! For example, a Life-Orb move that scores a critical hit, deals super-effective
//! damage, triggers the target's resist berry, and KOs the target looks like:
//!
//! ```text
//! MoveUsed { user, move_used, targets }
//!   ├── Crit { target }
//!   ├── SuperEffective { target }
//!   ├── DamageDealt { target (foe), amount, new_hp }
//!   │     └── ItemLost { slot (foe), item: resist_berry, consumed: true }
//!   │           └── Healed { target (foe), amount, new_hp }   ← berry heal
//!   ├── Faint { slot (foe) }
//!   ├── Healed { target (user), amount, new_hp }              ← drain
//!   └── DamageDealt { target (user), amount, new_hp }         ← Life Orb recoil
//! ```
//!
//! The parent event identifies the cause.
//! Child events do not need a separate source field.
//!
//! ## Priority / speed ordering
//!
//! The caller sorts top-level events by action priority and actor Speed.
//! Nested reactions remain with their cause.
//!
//! ## Integration with `unknowns`
//!
//! Events update [`crate::unknowns::UnknownBattleState`].
//! [`crate::unknowns::PokemonHP`] stores exact ally HP and opponent HP percentages.

use super::unknowns::PokemonHP;
use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    PokemonType, PseudoWeather, SideCondition, SlotCondition, Status, Terrain, VolatileStatus,
    Weather,
};

// ── Core recursive node ──────────────────────────────────────────────────────

/// Stores one revealed event and its direct reactions.
/// Reactions use battle resolution order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InformationEvent {
    pub kind: EventKind,
    /// Events directly caused by this event, in the order they resolve.
    pub reactions: Vec<InformationEvent>,
}

// ── Supporting types ─────────────────────────────────────────────────────────

/// Stores a Pokémon identity and visible state at entry.
/// Raw events contain true species and HP.
/// Masking uses `disguise_species` and `max_hp` for the opponent view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SwitchState {
    pub slot: FieldSlot,
    pub species: Species,
    pub level: u8,
    pub hp: PokemonHP,
    pub status: Option<Status>,
    /// `None` if not yet Terastallized or if the Tera type is not yet known.
    pub tera_type: Option<PokemonType>,
    /// The Illusion-disguised species a non-owning observer would see instead of
    /// `species`, if any (`None` = no active disguise, i.e. `species` is shown to
    /// everyone). Populated pre-mask; ignored after [`mask_events_for`] runs.
    pub disguise_species: Option<Species>,
    /// True max HP, needed by [`mask_events_for`] to compute a non-owning observer's
    /// `Percent` display from `hp`'s raw `Number`. Ignored after masking.
    pub max_hp: u16,
}

/// Reason a Pokémon could not act this turn.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CantReason {
    Flinch,
    Paralysis,
    Sleep,
    Freeze,
    Taunt,
    Disable,
    Encore,
    Recharge,
    Gravity,
    HealBlock,
    Imprison,
    Confusion,
    Bound,
    ThroatChop,
    Torment,
    SkyDrop,
    BeakBlast,
    FocusPunch,  // Hurt before Focus Punch fires
    Infatuation, // Attract — the Pokémon is infatuated and cannot act
    Other,
}

// ── EventKind ─────────────────────────────────────────────────────────────────

/// Every category of information a player can learn during a battle.
///
/// Variants are grouped by Showdown protocol category:
/// major actions, HP changes, hit qualifiers, status, stat stages,
/// field effects, volatile effects, items, and abilities.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventKind {
    // ── Major actions ────────────────────────────────────────────────────────
    /// A Pokémon successfully used a move.
    /// Reactions contain hit results, damage, secondaries, recoil, drain, etc.
    MoveUsed {
        user: FieldSlot,
        move_used: PokemonMove,
        /// Slots that were targeted (may be empty for moves that auto-target or miss
        /// before a target is chosen).
        targets: Vec<FieldSlot>,
    },

    /// A Pokémon entered the field (voluntary switch, or nested under its forcer).
    Switch(SwitchState),

    /// Multiple Pokémon entered the field simultaneously (battle-start leads or end-of-turn
    /// faint replacements). Ability activations (Intimidate, weather setters, etc.) and
    /// their downstream reactions are nested in this event's `reactions` in resolution order,
    /// exactly as the simulator processes them (`process_sendouts_in_speed_order_branching`).
    SimultaneousSwitch { switches: Vec<SwitchState> },

    /// Marks the end of a turn. Visible EOT effects (weather chip, Leftovers heal, etc.) are
    /// nested in `reactions`. The engine uses this event to trigger internal bookkeeping:
    /// timer decrements and per-turn/per-move volatile flag resets.
    EndOfTurn,

    /// A Pokémon fainted.
    Faint { slot: FieldSlot },

    /// A Pokémon Mega Evolved.
    MegaEvolution { slot: FieldSlot, into: Species },

    /// A Pokémon Terastallized.
    Terastallization {
        slot: FieldSlot,
        tera_type: PokemonType,
    },

    /// A Pokémon changed form. `permanent = true` for detailschange forms
    /// (Stance Change, Mimikyu-Busted, Palafin-Hero, etc.);
    /// `permanent = false` for temporary forme changes (-formechange).
    FormeChange {
        slot: FieldSlot,
        into: Species,
        permanent: bool,
    },

    /// A Pokémon's type(s) changed (Protean, Libero, Soak, Forest's Curse,
    /// Trick-or-Treat, Mimicry, Reflect Type, etc.).
    TypeChanged {
        slot: FieldSlot,
        new_types: Vec<PokemonType>,
    },

    /// A Pokémon could not act this turn.
    Cant { slot: FieldSlot, reason: CantReason },

    /// The first (charge) turn of a two-turn move. Emitted for BOTH families: the
    /// pure-charge moves (Solar Beam, Meteor Beam, Skull Bash, Geomancy, …) and the
    /// semi-invulnerable ones (Fly, Bounce, Dig, Dive, Phantom Force, Shadow Force,
    /// Sky Drop). The latter used to emit nothing, which left an observer unable to
    /// tell a takeoff from a landing.
    ///
    /// The target is deliberately *not* included: the charge step frequently reveals
    /// no target at all ("Charizard flew up high!" names nobody), and that is true of
    /// the pure-charge family too. It shows up on the enclosing `MoveUsed` if and when
    /// it is actually known.
    ///
    /// Always appears nested under the `MoveUsed` for the same move — `execute_action`
    /// folds everything a move emitted into `MoveUsed.reactions` — which is the tree
    /// shape tracker mode's grammar mirrors (`bin/server/tracker_parse.rs`).
    ChargingMove {
        user: FieldSlot,
        move_used: PokemonMove,
    },

    /// A Pokémon must recharge this turn (after Hyper Beam, Giga Impact, etc.).
    MustRecharge { slot: FieldSlot },

    /// A single-turn or single-move effect was announced
    /// (Protect, Detect, King's Shield, Focus Punch charge, Beak Blast, etc.).
    SingleMoveOrTurn {
        slot: FieldSlot,
        move_used: PokemonMove,
    },

    // ── HP changes ───────────────────────────────────────────────────────────
    //
    // Two primitives cover every HP delta; the cause is implied by the enclosing
    // parent event (move, item, ability, field effect, etc.).
    //
    // Recoil and Life Orb damage: `DamageDealt { target == user … }`
    // Drain healing:              `Healed { target == user … }`
    // Crash damage (e.g. HJK):   `DamageDealt { target == user … }` under the MoveUsed
    /// HP was lost. Covers move damage, recoil, Life Orb, crash, confusion self-hit,
    /// entry hazards, weather chip, status chip, binding/Salt Cure/Leech Seed residual,
    /// and Future Sight. Whether it is self-inflicted is determined by `target`.
    ///
    /// `new_hp` is the *raw* value pre-mask (always `PokemonHP::Number`, the true HP);
    /// `max_hp` accompanies it so [`mask_events_for`] can derive `Percent` for a
    /// non-owning observer. Both fields are only meaningful before masking runs.
    DamageDealt {
        target: FieldSlot,
        new_hp: PokemonHP,
        max_hp: u16,
    },

    /// HP was restored. Covers healing moves, drain, Leftovers, berries, Aqua Ring,
    /// Ingrain, Wish, Regenerator, and any other recovery. Drain healing sits as a
    /// reaction of the enclosing `MoveUsed` with `target == user`.
    ///
    /// See [`DamageDealt`] for the `new_hp`/`max_hp` raw-vs-masked contract.
    Healed {
        target: FieldSlot,
        new_hp: PokemonHP,
        max_hp: u16,
    },

    /// HP was set to an exact value by a move (Pain Split). Use [`DamageDealt`] /
    /// [`Healed`] for ordinary deltas; this is only for direct-set effects.
    ///
    /// See [`DamageDealt`] for the `new_hp`/`max_hp` raw-vs-masked contract.
    SetHp {
        target: FieldSlot,
        new_hp: PokemonHP,
        max_hp: u16,
    },

    // ── Hit qualifiers (reactions of MoveUsed) ───────────────────────────────
    /// The preceding hit scored a critical hit.
    Crit { target: FieldSlot },

    /// The target was completely immune to the move. When the cause is hidden
    /// information (an ability or item), this event is nested as a reaction *under*
    /// an enclosing [`AbilityRevealed`]/[`ItemRevealed`] — the reveal is the cause,
    /// Immune is its consequence, matching [`MoveFailed`]'s convention. A bare
    /// top-level Immune with no enclosing reveal means plain type-chart immunity
    /// (already-public typing) with nothing further to explain.
    Immune { target: FieldSlot },

    /// The move missed its target.
    Missed { target: FieldSlot },

    /// The move failed at the move-effect level (not a miss). Cause is conveyed by
    /// the enclosing context (e.g. a nested `AbilityRevealed` for Soundproof blocking).
    MoveFailed { slot: FieldSlot },

    /// The move was blocked (e.g. Protect, King's Shield, Baneful Bunker, Crafty Shield,
    /// Substitute absorbing). A nested [`AbilityRevealed`] or [`VolatileStart`] may
    /// elaborate.
    Blocked { target: FieldSlot },

    /// A multi-hit move struck this many times.
    HitCount { target: FieldSlot, hits: u8 },

    // ── Status conditions ────────────────────────────────────────────────────
    /// A non-volatile status condition was applied (Burn, Paralysis, Sleep, Poison,
    /// Toxic, Freeze).
    StatusInflicted { target: FieldSlot, status: Status },

    /// A non-volatile status condition was cured (Lum Berry, Natural Cure, defrost,
    /// single-mon cure paths, etc.). Cause conveyed by nesting.
    StatusCured { target: FieldSlot, status: Status },

    /// Heal Bell / Aromatherapy cured the entire side's team (active + bench) at once.
    /// Used because benched Pokémon have no on-field `FieldSlot` addressable identity.
    TeamStatusCured { side: Player },

    // ── Stat stages ──────────────────────────────────────────────────────────
    /// A stat stage changed. `stages` is signed: positive = boost, negative = drop.
    /// Covers `-boost`, `-unboost`, and `-setboost` (set is expressed as an absolute
    /// stage value relative to the current stage when constructing the event).
    ///
    /// `boost_idx` is the index into the `PokemonBoostTable` / `[i8; 7]` boost array:
    ///   0=Atk, 1=Def, 2=SpA, 3=SpD, 4=Spe, 5=Accuracy, 6=Evasion
    BoostChanged {
        target: FieldSlot,
        boost_idx: usize,
        stages: i8,
    },

    /// All stat stages were reset to 0 (e.g. Haze, Clear Smog on target).
    BoostsCleared { target: FieldSlot },

    /// All stat stages were inverted (positive ↔ negative) by Topsy-Turvy.
    BoostsInverted { target: FieldSlot },

    /// Stat stages were swapped between two Pokémon (Guard Swap / Heart Swap /
    /// Power Swap — specific stat subset implied by the causing move).
    BoostsSwapped {
        source: FieldSlot,
        target: FieldSlot,
    },

    /// One Pokémon's stat stages were copied onto another (Psych Up).
    BoostsCopied {
        source: FieldSlot,
        target: FieldSlot,
    },

    // ── Field: weather / terrain / pseudo-weather / side / slot ─────────────
    /// The weather changed. `None` means weather ended or was cleared.
    WeatherChanged { weather: Option<Weather> },

    /// The terrain changed. `None` means terrain ended or was cleared.
    TerrainChanged { terrain: Option<Terrain> },

    /// A pseudo-weather effect (Gravity, Trick Room, Wonder Room, etc.) started.
    PseudoWeatherStart { effect: PseudoWeather },

    /// A pseudo-weather effect ended.
    PseudoWeatherEnd { effect: PseudoWeather },

    /// A side condition was established (Reflect, Light Screen, Spikes layer, Stealth
    /// Rock, Tailwind, Safeguard, etc.).
    SideConditionStart {
        side: Player,
        condition: SideCondition,
    },

    /// A side condition ended or was removed.
    SideConditionEnd {
        side: Player,
        condition: SideCondition,
    },

    /// A slot condition was established (Future Sight incoming, Wish, Healing Wish,
    /// Lunar Dance, Revival Blessing).
    SlotConditionStart {
        slot: FieldSlot,
        condition: SlotCondition,
    },

    /// A slot condition resolved or was cleared.
    SlotConditionEnd {
        slot: FieldSlot,
        condition: SlotCondition,
    },

    // ── Volatile status effects ──────────────────────────────────────────────
    /// A volatile status was inflicted (Confusion, Taunt, Flinch, Leech Seed,
    /// Substitute, Encore, Disable, etc.).
    VolatileStart {
        target: FieldSlot,
        volatile: VolatileStatus,
    },

    /// A volatile status ended (Taunt wore off, Confusion cured, Substitute broken,
    /// etc.).
    VolatileEnd {
        target: FieldSlot,
        volatile: VolatileStatus,
    },

    /// Perish Song countdown ticked — reveals current remaining turns.
    PerishCount { target: FieldSlot, turns_left: u8 },

    // ── Items ────────────────────────────────────────────────────────────────
    //
    // Three primitives map to the three fields in UnknownPokemonState:
    //   ItemRevealed → `item` field now known (no state change for the item itself)
    //   ItemGained   → `item` field changed to a different item
    //   ItemLost     → `item` set to None; if consumed=true also set `consumed_item`
    /// The Pokémon's held item was revealed without being consumed or transferred
    /// (switch-in announce, Frisk, Air Balloon pop reveal, item activating visibly).
    ItemRevealed { slot: FieldSlot, item: Item },

    /// The Pokémon acquired a held item (or had it replaced). Covers Trick, Switcheroo,
    /// Symbiosis, Recycle, Pickup, Thief (gainer side).
    ItemGained { slot: FieldSlot, item: Item },

    /// The Pokémon's held item was removed. `consumed = true` means the item was used
    /// up by the holder (berry eaten, gem expended, eject button triggered, etc.) and
    /// should set `consumed_item`; `false` means it was taken/removed externally
    /// (Knock Off, Incinerate, Thief loser side, Fling) and should set `item_lost`.
    ItemLost {
        slot: FieldSlot,
        item: Item,
        consumed: bool,
    },

    // ── Abilities ────────────────────────────────────────────────────────────
    /// The Pokémon's ability was revealed or changed. Covers switch-in announces,
    /// Trace, Skill Swap result, Mummy, Wandering Spirit, Mega Evolution ability,
    /// and any other ability disclosure.
    AbilityRevealed { slot: FieldSlot, ability: Ability },
    // NOTE: Ability suppression is NOT a discrete event; it is tracked as state via
    // the GastroAcid volatile on the affected Pokémon and a field-wide NeutralizingGas scan,
    // exactly mirroring `pokemon_ability_is_suppressed` in simulator/helpers.rs.

    // ── Information abilities ─────────────────────────────────────────────────
    /// Anticipation fired on switch-in: an opposing active Pokémon has a super-effective
    /// or OHKO move against this holder. `slot` is the Anticipation holder.
    /// A nested `AbilityRevealed { slot, ability: Anticipation }` is always present.
    AnticipationShudder { slot: FieldSlot },

    /// An Illusion disguise was dispelled, revealing the holder's true species.
    /// Emitted as a sibling of `DamageDealt` on the first direct-move damage hit,
    /// or alongside ability suppression/change events.
    IllusionEnded {
        slot: FieldSlot,
        actual_species: Species,
    },

    /// A Pokémon Transformed into another (the Transform move or the Imposter ability).
    /// `slot` is the transformer; `into_slot` is the copied Pokémon (the directly-
    /// opposite active slot); `into_species` is the displayed species after the copy.
    ///
    /// The transformer adopts the copy source's species, types, stats (except HP /
    /// max HP), ability, moves (PP capped at 5), and stat stages; its own level, HP,
    /// item, status, nature, EVs, and IVs are unchanged. Inference reads the copy
    /// source's *fog* entry at `into_slot`, so transforming into the observer's own
    /// Pokémon yields exact copied stats while transforming into a hidden opponent
    /// inherits that opponent's current bounds.
    Transformed {
        slot: FieldSlot,
        into_slot: FieldSlot,
        into_species: Species,
    },
}

// ── Masking: raw resolution → per-observer view ────────────────────────────────

/// Downgrade a **raw** (unmasked) event tree — produced by a single turn resolution
/// with event tracking on — into the stream a specific `observer` would actually see.
///
/// This is the sound alternative to resolving a turn twice with different observers:
/// `sample_turn`/`simulate_turn` pick a weighted-*random* trajectory (damage rolls, crit,
/// status durations, `sample_one_weighted`), so two independent resolutions would follow
/// two different random universes and could not be soundly attributed to the same
/// `next_state`. Instead the turn is resolved **once**, tracking exact HP/`max_hp` and
/// true+disguise species everywhere; `mask_events_for` is then a **pure, deterministic**
/// transform of that one trajectory, callable once per player to get both perspectives.
///
/// It only *rewrites* the handful of perspective-sensitive fields — `PokemonHP` on
/// `DamageDealt`/`Healed`/`SetHp`/`SwitchState`, and `SwitchState.species` under Illusion —
/// never drops a node: every other `EventKind` (statuses, boosts, items, abilities, field
/// effects, `IllusionEnded`, `Transformed`, …) is public to both players in this engine's
/// model and passes through unchanged. Reactions are recursed in place.
///
/// Feeding `mask_events_for(Player::P1, raw)` must reproduce exactly what
/// `sample_turn(..., Some(Player::P1))` returned before this refactor — that parity is the
/// regression anchor (see `mask_events_for_p1_parity` in the test suite). The P2 stream is
/// simply the same transform with the observer flipped.
pub fn mask_events_for(observer: Player, events: &[InformationEvent]) -> Vec<InformationEvent> {
    mask_events(Perspective::Player(observer), events)
}

/// Downgrade the same **raw** event tree into the stream that a spectator sees:
/// a percentage for every HP, and the disguise species of every Illusion user.
///
/// This is not two applications of [`mask_events_for`]. The first application
/// clears `disguise_species`, so a second one would restore the true species of
/// an Illusion user on the side that the first observer owned. The public view
/// therefore needs its own pass over the raw stream.
///
/// The public stream is the common knowledge of the turn. A search that needs
/// what both players learned in common — and nothing that either player learned
/// alone — reads this instead of intersecting the two private streams.
pub fn mask_events_public(events: &[InformationEvent]) -> Vec<InformationEvent> {
    mask_events(Perspective::Public, events)
}

/// Whose view a mask builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Perspective {
    /// One player's own view. That player's slots keep exact HP and true species.
    Player(Player),
    /// A spectator's view. No slot is owned, so every slot is masked.
    Public,
}

impl Perspective {
    /// Whether this perspective owns the side that `slot_player` plays.
    fn owns(self, slot_player: Player) -> bool {
        matches!(self, Perspective::Player(observer) if observer == slot_player)
    }
}

fn mask_events(view: Perspective, events: &[InformationEvent]) -> Vec<InformationEvent> {
    events.iter().map(|ev| mask_event(view, ev)).collect()
}

fn mask_event(view: Perspective, ev: &InformationEvent) -> InformationEvent {
    let kind = mask_event_kind(view, &ev.kind);
    let reactions = ev.reactions.iter().map(|r| mask_event(view, r)).collect();
    InformationEvent { kind, reactions }
}

/// `raw` is the true HP as `PokemonHP::Number` (pre-mask contract); `max_hp` is its
/// companion. Returns the observer-appropriate `Number` (own slot) or `Percent` (foe slot).
fn mask_hp(view: Perspective, slot_player: Player, raw: &PokemonHP, max_hp: u16) -> PokemonHP {
    let hp = match raw {
        PokemonHP::Number(n) => *n,
        // Already masked (shouldn't occur pre-mask, but stay sound if it ever does).
        PokemonHP::Percent(p) => return PokemonHP::Percent(*p),
    };
    if view.owns(slot_player) {
        PokemonHP::Number(hp)
    } else {
        PokemonHP::Percent(crate::simulator::helpers::hp_to_percent(hp, max_hp))
    }
}

fn mask_switch_state(view: Perspective, raw: &SwitchState) -> SwitchState {
    let species = if view.owns(raw.slot.player) {
        raw.species.clone()
    } else {
        raw.disguise_species
            .clone()
            .unwrap_or_else(|| raw.species.clone())
    };
    let hp = mask_hp(view, raw.slot.player, &raw.hp, raw.max_hp);
    SwitchState {
        slot: raw.slot,
        species,
        level: raw.level,
        hp,
        status: raw.status.clone(),
        tera_type: raw.tera_type.clone(),
        // Vestigial post-mask; nothing downstream reads these once `hp`/`species` are set.
        disguise_species: None,
        max_hp: raw.max_hp,
    }
}

fn mask_event_kind(view: Perspective, kind: &EventKind) -> EventKind {
    match kind {
        EventKind::DamageDealt {
            target,
            new_hp,
            max_hp,
        } => EventKind::DamageDealt {
            target: *target,
            new_hp: mask_hp(view, target.player, new_hp, *max_hp),
            max_hp: *max_hp,
        },
        EventKind::Healed {
            target,
            new_hp,
            max_hp,
        } => EventKind::Healed {
            target: *target,
            new_hp: mask_hp(view, target.player, new_hp, *max_hp),
            max_hp: *max_hp,
        },
        EventKind::SetHp {
            target,
            new_hp,
            max_hp,
        } => EventKind::SetHp {
            target: *target,
            new_hp: mask_hp(view, target.player, new_hp, *max_hp),
            max_hp: *max_hp,
        },
        EventKind::Switch(sw) => EventKind::Switch(mask_switch_state(view, sw)),
        EventKind::SimultaneousSwitch { switches } => EventKind::SimultaneousSwitch {
            switches: switches
                .iter()
                .map(|sw| mask_switch_state(view, sw))
                .collect(),
        },
        // No perspective-sensitive payload — every other event category (major actions
        // sans HP, hit qualifiers, status, boosts, field effects, items, abilities,
        // IllusionEnded, Transformed) is public to both players in this engine's model.
        other => other.clone(),
    }
}
