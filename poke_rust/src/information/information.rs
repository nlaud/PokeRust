//! # Information — the player-visible event model
//!
//! This module defines every event that **reveals information to a player** during a
//! Pokémon battle. It is modelled on Showdown's SIM-PROTOCOL but differs in one key
//! structural way: **reaction events are nested inside the event that triggered them**
//! via the [`InformationEvent::reactions`] field.
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
//! Because the *parent* event already conveys the cause (e.g. the enclosing `MoveUsed`
//! tells you that recoil came from a move), there is no separate "effect source" tag on
//! individual events.
//!
//! ## Priority / speed ordering
//!
//! Ordering top-level events by action priority and actor speed is **not done here** —
//! it is the responsibility of the caller that assembles a `Vec<InformationEvent>` for
//! a turn. The nested structure means each reaction automatically travels with its cause
//! regardless of ordering.
//!
//! ## Integration with `unknowns`
//!
//! This is the bridge to [`crate::unknowns::UnknownBattleState`]. All HP figures use
//! [`crate::unknowns::PokemonHP`] so that allies report exact HP and opponents report a
//! percentage, exactly as a real player observes.

use crate::state::battle::{FieldSlot, Player};
use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::dex_data::{
    PokemonType, PseudoWeather, SideCondition, SlotCondition, Status, Terrain, VolatileStatus,
    Weather,
};
use super::unknowns::PokemonHP;

// ── Core recursive node ──────────────────────────────────────────────────────

/// A single piece of information revealed to a player, together with any further
/// events it caused. Reactions are ordered by their in-battle resolution sequence.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InformationEvent {
    pub kind: EventKind,
    /// Events directly caused by this event, in the order they resolve.
    pub reactions: Vec<InformationEvent>,
}

// ── Supporting types ─────────────────────────────────────────────────────────

/// The identity and visible state of a Pokémon as it enters the field.
/// Used for voluntary switches and — when nested under a causing event — forced switches
/// (Roar, Whirlwind, Dragon Tail, Red Card, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SwitchState {
    pub slot: FieldSlot,
    pub species: Species,
    pub level: u8,
    pub hp: PokemonHP,
    pub status: Option<Status>,
    /// `None` if not yet Terastallized or if the Tera type is not yet known.
    pub tera_type: Option<PokemonType>,
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
    FocusPunch, // Hurt before Focus Punch fires
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

    /// A charging move was declared. The target is *not* included because it is not
    /// always revealed at the charge step (e.g. Fly, Dive, Phantom Force).
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
    DamageDealt {
        target: FieldSlot,
        new_hp: PokemonHP,
    },

    /// HP was restored. Covers healing moves, drain, Leftovers, berries, Aqua Ring,
    /// Ingrain, Wish, Regenerator, and any other recovery. Drain healing sits as a
    /// reaction of the enclosing `MoveUsed` with `target == user`.
    Healed {
        target: FieldSlot,
        new_hp: PokemonHP,
    },

    /// HP was set to an exact value by a move (Pain Split). Use [`DamageDealt`] /
    /// [`Healed`] for ordinary deltas; this is only for direct-set effects.
    SetHp {
        target: FieldSlot,
        new_hp: PokemonHP,
    },

    // ── Hit qualifiers (reactions of MoveUsed) ───────────────────────────────
    /// The preceding hit scored a critical hit.
    Crit { target: FieldSlot },

    /// The target was completely immune to the move. A nested [`AbilityRevealed`] or
    /// [`TypeChanged`] may explain *why*.
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
    IllusionEnded { slot: FieldSlot, actual_species: Species },

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
