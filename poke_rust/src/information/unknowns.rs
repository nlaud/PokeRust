use crate::data::ability::Ability;
use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::battle::{FieldSlot, Player};
use crate::state::dex_data::{
    PokemonBoostTable, PokemonData, PokemonStat, PokemonType, PseudoWeather, SelfSwitchType,
    SideCondition, SlotCondition, Status, Terrain, Weather,
};
use crate::state::pokemon::{
    Nature, PokemonGender, PokemonState, PokemonStatsTable, VolatileStatusState, calc_hp, calc_stat,
};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Unknown<T> {
    Known(T),
    Not(Vec<T>),
    Possibly(Vec<T>),
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PokemonHP {
    Number(u16), //Allies use number
    Percent(u8), //Opponents use percent
}

#[derive(Debug, Clone)]
pub struct UnknownPokemonState {
    pub possible_mon_id: Unknown<u8>, //1:1 with species if there is only 1 of each pokemon
    pub fainted: bool,
    pub possible_species: Unknown<Species>,
    pub possible_types: Unknown<Vec<PokemonType>>, //These two fields correspont 1:1
    pub is_tera: bool,
    pub is_mega: bool,

    pub level: u8,
    pub hp: PokemonHP,
    pub known_moves: [Option<PokemonMove>; 4], //None = We don't know this move
    pub move_pp: [i8; 4],                      //-1 = We don't know this move's PP
    pub max_pp: [i8; 4],                       //-1 = We don't know this move's max PP
    pub item: Unknown<Item>,
    /// The last item consumed by this Pokémon (set when eating a Berry or consuming an item).
    /// Used by Harvest to restore a Berry, and Recycle to recover any consumed item.
    /// Not set for items lost via Knock Off, theft, or other non-consumption means.
    pub consumed_item: Option<Item>,
    /// Cud Chew delayed re-eat: `Some((berry, armed))`.
    /// `armed=false` means one EOT has not yet passed; `armed=true` means the re-eat fires
    /// this EOT. Cleared on switch-out or when the re-eat fires.
    pub cud_chew_pending: Option<(Item, bool)>,
    /// True once this Pokémon's held item has been consumed or removed while it was on the
    /// field, and no replacement item has been gained since. Powers Unburden's ×2 Speed.
    /// Cleared on switch-out and whenever an item is gained.
    pub item_lost: bool,
    /// Item revealed when it was taken or knocked off (Knock Off, Thief loser side, Fling).
    /// Not set for consumed items (use `consumed_item`) or Trick/Switcheroo (use `item`).
    pub removed_item: Option<Item>,

    // ── Per-turn event flags (cleared in end_turn Phase 5 and on switch-out) ────────
    /// Took any damage this turn — direct hits, recoil, confusion self-hits, etc.
    /// Read by Assurance's ×2 condition.
    pub damaged_this_turn: bool,
    /// Slots whose direct move hits damaged this Pokémon this turn. Read by Avalanche
    /// ("the target damaged the user this turn").
    pub damaged_by_this_turn: Vec<crate::state::battle::FieldSlot>,
    /// Damage taken from the most recent physical direct-move hit this turn, and the slot
    /// that landed it. Overwritten per-hit (multi-hit: final hit wins). Read by Counter.
    pub last_physical_damage_taken: PokemonHP,
    pub last_physical_attacker: Option<crate::state::battle::FieldSlot>,
    /// Damage taken from the most recent special direct-move hit this turn, and the slot
    /// that landed it. Read by Mirror Coat.
    pub last_special_damage_taken: PokemonHP,
    pub last_special_attacker: Option<crate::state::battle::FieldSlot>,
    /// Damage taken from the most recent direct-move hit this turn (any category), and the
    /// slot that landed it. Read by Metal Burst and Comeuppance.
    pub last_damage_taken: PokemonHP,
    pub last_damage_attacker: Option<crate::state::battle::FieldSlot>,
    /// Any stat stage actually rose this turn (post-clamp). Gates Burning Jealousy's burn.
    pub stats_raised_this_turn: bool,
    /// Any stat stage actually fell this turn (post-clamp). Read by Lash Out's ×2.
    pub stats_lowered_this_turn: bool,
    /// Entered the field via a switch THIS turn (not battle-start leads — those only set
    /// `entered_this_turn`). Payback does not double against a Pokémon that switched in.
    pub switched_in_this_turn: bool,
    /// Consecutive successful stalling-move (Protect/Detect/Endure/King's Shield/Spiky Shield/
    /// Baneful Bunker) uses. Drives the 1/3^n success decay. Reset by any non-stalling move, a
    /// failed stall, switch-out, and (best-effort) "couldn't act" cases.
    pub stall_counter: u8,
    /// Consecutive successful Ally Switch uses. Independent from stall_counter (the two
    /// decay chains are separate per Bulbapedia). Reset on non-Ally-Switch move, failed
    /// Ally Switch, or switch-out.
    pub ally_switch_counter: u8,

    pub possible_natures: Unknown<Nature>,
    pub minEvs: [u8; 6],
    pub maxEvs: [u8; 6],
    pub minIvs: [u8; 6],
    pub maxIvs: [u8; 6],

    pub minStats: PokemonStatsTable,
    pub maxStats: PokemonStatsTable,

    /// Pre-nature base stat value bounds: `calc_stat(base, iv, ev, level, 1.0)` before ×0.9/1.0/1.1.
    /// Tightened by Pass 3 (damage→stat inversion). Used by Pass 5 + Pass 6 `EVIVStatGE`/`EVIVStatLE`.
    /// Index matches `minStats`/`maxStats`: 0=HP, 1=Atk, 2=Def, 3=SpA, 4=SpD, 5=Spe.
    pub min_pre_nature_stat: PokemonStatsTable,
    pub max_pre_nature_stat: PokemonStatsTable,

    pub boosts: PokemonBoostTable,
    pub status: Option<Status>,

    pub volatiles: Vec<VolatileStatusState>, //If this might be unkown, replace it with Unknown<Vec<VolatileStatusState>>

    pub possible_original_abilities: Unknown<Ability>,
    pub possible_abilities: Unknown<Ability>,

    pub possible_genders: Unknown<PokemonGender>,
    pub possible_weight_hg: Unknown<u16>, //Also 1:1 with species

    pub possible_tera_type: Unknown<PokemonType>,

    pub mega_species: Unknown<Option<Species>>, //1:1 with species
    pub mega_ability: Unknown<Option<Ability>>, //1:1 with species

    pub last_move_failed: bool, //For stomping tantrum

    pub last_used_move: Option<PokemonMove>,
    /// Consecutive uses of the same move (for the Metronome item boost).
    /// Incremented when the same move succeeds back-to-back; reset on a different move,
    /// a miss, or switching out.  Saturates at 255 (well above the cap of 5).
    pub consecutive_move_count: u8,
    /// Tracks which move slots have been used since this Pokémon was sent in (for Last Resort).
    /// Index matches `moves`; cleared on switch-in alongside `first_move_on_field`.
    pub used_moves_this_field: [bool; 4],

    /// Generic once-per-battle flag for abilities that fire only on first entry
    /// (e.g. Supersweet Syrup, Intrepid Sword). Persists across switch-outs
    pub one_time_ability_used: bool,

    /// True once this Pokémon has eaten any Berry this battle (own berry, Bug Bite/Pluck,
    /// Fling). Persists across switch-out, faint+revive, and Recycle. Required by Belch.
    pub ate_berry_this_battle: bool,

    /// True during the first TURN the Pokémon can act. Set on any entry; cleared at end-of-turn.
    /// For U-turn/self-switch mid-turn entries (turn_started=true, turn_ended=false), the flag
    /// must survive the current turn's EOT and remain true the NEXT turn. That is handled by
    /// `first_turn_on_field_pending` (see below). Gates Fake Out and First Impression.
    pub first_move_on_field: bool,

    /// Set in process_pokemon_send_out when a Pokémon enters MID-TURN before EOT has run
    /// (U-turn / Volt Switch / Parting Shot self-switch; detected via turn_started && !turn_ended).
    /// Tells EOT Phase 5 to skip clearing first_move_on_field for exactly one EOT cycle, so the
    /// flag persists to the NEXT turn (the Pokémon's actual first chance to act).
    pub first_turn_on_field_pending: bool,

    /// True on the turn this Pokémon entered battle via a SwitchAction (voluntary or forced
    /// mid-turn). Cleared at the end of end_turn after ability effects are applied.
    /// Used by Speed Boost to skip the boost on the entry turn.
    /// NOT set for faint replacements (which enter after end_turn has already run for
    /// the KO turn and should receive Speed Boost normally on their first end_turn).
    pub entered_this_turn: bool,

    /// Saved pre-transform snapshot for Transform revert. Boxed to avoid
    /// an infinite-size struct (recursive types require indirection in Rust).
    pub pre_transform: Option<Box<UnknownPokemonState>>, //Don't need to make this unknown unless pokemon with imposter + transform exists

    /// Original types saved when Mimicry overwrites them. Restored when terrain ends or
    /// the holder switches out. `None` when Mimicry is not active.
    pub pre_mimicry_types: Option<Vec<crate::state::dex_data::PokemonType>>,

    /// Hit counter for Rage Fist. Incremented each time this Pokémon is hit by a direct damaging
    /// move (from any source, including allies). Reset on switch-out or faint (Champions rules);
    /// never reset at end-of-turn while the mon remains on field.
    pub times_hit: u16,
}

#[derive(Debug, Clone)]
pub struct UnknownBattleState {
    pub active_per_side: u8,
    pub back_mons_per_side: u8,

    pub p1_active_mons: Vec<UnknownPokemonState>,
    pub p2_active_mons: Vec<UnknownPokemonState>,
    pub p1_known_back_mons: Vec<UnknownPokemonState>,
    pub p2_known_back_mons: Vec<UnknownPokemonState>,
    pub p1_possible_back_mons: Vec<UnknownPokemonState>,
    pub p2_possible_back_mons: Vec<UnknownPokemonState>,

    pub turn_number: u16,

    //Both false = waiting for moves from both players
    //Started true, ended false = processing actions from action_queue
    //Both true = check if players have active fainted mons to send out
    pub turn_started: bool,
    pub turn_ended: bool,

    pub p1_has_tera: bool,
    pub p2_has_tera: bool,

    pub p1_has_mega: bool,
    pub p2_has_mega: bool,

    pub weather: Option<Weather>,
    pub weather_turns: Option<Unknown<u8>>,
    pub pseudo_weathers: Vec<PseudoWeather>,
    pub pseudo_weather_turns: Vec<Unknown<u8>>,
    pub terrain: Option<Terrain>,
    pub terrain_turns: Option<Unknown<u8>>,
    pub p1_side_conditions: Vec<SideCondition>,
    pub p1_side_condition_turns: Vec<Unknown<u8>>,
    pub p2_side_conditions: Vec<SideCondition>,
    pub p2_side_condition_turns: Vec<Unknown<u8>>,
    pub p1_slot_conditions: Vec<Vec<SlotCondition>>,
    pub p2_slot_conditions: Vec<Vec<SlotCondition>>,

    /// Set mid-turn after a self-switch move (U-turn, Baton Pass, etc.) fully resolves and the
    /// user is alive with a healthy bench.  While this is `Some`, `simulate_turn` returns to the
    /// caller so the player can choose a replacement; only the pending slot may switch, every
    /// other active slot must Pass.  Cleared once the replacement is sent in.
    pub self_switch_pending: Option<(FieldSlot, SelfSwitchType)>,

    /// One-time-use items consumed this turn, as `(consumer slot, item)` in consumption
    /// order. Pickup takes the most recent entry consumed by *another* Pokémon at end of
    /// turn; Harvest removes its restored berry from this pool. Cleared at the end of
    /// every `end_turn`. Items removed by theft (and, in future, Knock Off / Incinerate)
    /// are deliberately NOT recorded.
    pub items_consumed_this_turn: Vec<(FieldSlot, Item)>,

    /// The last move successfully executed by any Pokémon on the field (either side).
    /// Updated whenever `last_used_move` is set on a mon. Used by Copycat.
    pub last_move_on_field: Option<PokemonMove>,

    /// Damage dealt to a Substitute this action (the full damage roll, not the sub HP
    /// absorbed). Used by `apply_post_damage_move_effects` to compute recoil correctly
    /// when the sub had less HP than the damage roll. Excluded from PartialEq/Hash.
    /// Reset to 0 after recoil is applied.
    pub sub_damage_dealt: u32,

    /// Set to true after the first Round resolves this turn; causes subsequent Rounds
    /// to deal doubled base power. Cleared at end of turn. Excluded from PartialEq/Hash.
    pub round_used_this_turn: bool,

    pub predicates: Vec<Vec<Statement>>, //AND of ORs
}

/// A literal fact about the battle state.
///
/// ## `mon_idx` indexing convention
///
/// `mon_idx` uniquely identifies a Pokémon within a flat list derived from
/// `UnknownBattleState` in this order:
///
/// ```text
/// [p1_active_mons..., p1_known_back_mons..., p1_possible_back_mons...,
///  p2_active_mons..., p2_known_back_mons..., p2_possible_back_mons...]
/// ```
///
/// For `UnknownTeamPreviewState` the list is simply `[p1_mons..., p2_mons...]`.
///
/// Use `mon_idx_for_slot()` and `get_mon_by_idx()` / `get_mon_mut_by_idx()`
/// (defined in `information::inference`) to resolve indices.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Not(Box<Statement>),
    HasItem {
        mon_idx: usize,
        item: Item,
    },
    HasStatus {
        mon_idx: usize,
        status: Status,
    },
    HasMove {
        mon_idx: usize,
        pokemon_move: PokemonMove,
    },
    HasAbility {
        mon_idx: usize,
        ability: Ability,
    },
    WeatherTurns {
        turns: usize,
    },
    PseudoWeatherTurns {
        turns: usize,
    },
    SideConditionTurns {
        side: Player,
        side_condition: SideCondition,
        turns: usize,
    },
    NatureBoostsStat {
        mon_idx: usize,
        stat: PokemonStat,
    },
    NatureNerfsStat {
        mon_idx: usize,
        stat: PokemonStat,
    },
    /// PRE-nature base stat value (calc_stat with mod=1.0) ≥ `value`.
    /// Emitted by Pass 3. Checked and forced against `min_pre_nature_stat`/`max_pre_nature_stat`.
    EVIVStatGE {
        mon_idx: usize,
        stat: PokemonStat,
        value: u16,
    },
    /// PRE-nature base stat value (calc_stat with mod=1.0) ≤ `value`.
    /// Symmetric upper-bound companion to `EVIVStatGE`.
    EVIVStatLE {
        mon_idx: usize,
        stat: PokemonStat,
        value: u16,
    },
    /// Cross-mon effective-speed comparison in the same priority bracket.
    ///
    /// Invariant: `base_spe(fast_idx) * fast_mult >= base_spe(slow_idx) * slow_mult`
    ///
    /// `fast_mult` and `slow_mult` encode the product of all *observable* speed
    /// multipliers (boost stages, paralysis, Tailwind, weather-speed abilities, etc.)
    /// at the moment the ordering was observed, scaled to a common integer denominator.
    /// Trick Room is already handled by swapping `fast_idx`/`slow_idx`.
    ///
    /// Worlds where hidden speed items (Iron Ball, Choice Scarf) or random sources
    /// (Quick Claw, Quick Draw) could explain the ordering are emitted as **separate
    /// disjunctive clauses** alongside this one, not folded into the multipliers here.
    SpeedComparison {
        fast_idx: usize,
        slow_idx: usize,
        fast_mult: u32,
        slow_mult: u32,
    },
}

// ── Team-preview fog-of-war state ─────────────────────────────────────────────

/// The fog-of-war view of both teams at team preview.  Mirrors `battle::TeamPreviewState`
/// but carries `UnknownPokemonState` entries so one side can be fully known while the
/// other is species-only.
#[derive(Debug, Clone)]
pub struct UnknownTeamPreviewState {
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub p1_mons: Vec<UnknownPokemonState>,
    pub p2_mons: Vec<UnknownPokemonState>,
}

/// Fog-of-war analogue of `battle::MatchState`.  Tracks the game phase from a single
/// player's perspective so observation code can pattern-match symmetrically with the
/// concrete state machine.
#[derive(Debug, Clone)]
pub enum UnknownMatchState {
    TeamPreview(UnknownTeamPreviewState),
    Battle(UnknownBattleState),
    GameOver { winner: Player },
}

// ── UnknownPokemonState constructors ──────────────────────────────────────────

impl UnknownPokemonState {
    /// Build a fully-known fog-of-war view from one of **your own** Pokémon.
    /// Every `Unknown<T>` field is set to `Known(…)` and every scalar is copied directly.
    pub fn from_known_pokemon(mon: &PokemonState) -> Self {
        UnknownPokemonState {
            possible_mon_id: Unknown::Known(mon.mon_id),
            fainted: mon.fainted,
            possible_species: Unknown::Known(mon.species.clone()),
            possible_types: Unknown::Known(mon.types.clone()),
            is_tera: mon.is_tera,
            is_mega: mon.is_mega,
            level: mon.level,
            hp: PokemonHP::Number(mon.hp),
            known_moves: mon.moves.clone(),
            move_pp: [
                mon.move_pp[0] as i8,
                mon.move_pp[1] as i8,
                mon.move_pp[2] as i8,
                mon.move_pp[3] as i8,
            ],
            max_pp: [
                mon.max_pp[0] as i8,
                mon.max_pp[1] as i8,
                mon.max_pp[2] as i8,
                mon.max_pp[3] as i8,
            ],
            item: Unknown::Known(mon.item.clone()),
            consumed_item: mon.consumed_item.clone(),
            cud_chew_pending: mon.cud_chew_pending.clone(),
            item_lost: mon.item_lost,
            removed_item: None,
            damaged_this_turn: mon.damaged_this_turn,
            damaged_by_this_turn: mon.damaged_by_this_turn.clone(),
            last_physical_damage_taken: PokemonHP::Number(mon.last_physical_damage_taken),
            last_physical_attacker: mon.last_physical_attacker,
            last_special_damage_taken: PokemonHP::Number(mon.last_special_damage_taken),
            last_special_attacker: mon.last_special_attacker,
            last_damage_taken: PokemonHP::Number(mon.last_damage_taken),
            last_damage_attacker: mon.last_damage_attacker,
            stats_raised_this_turn: mon.stats_raised_this_turn,
            stats_lowered_this_turn: mon.stats_lowered_this_turn,
            switched_in_this_turn: mon.switched_in_this_turn,
            stall_counter: mon.stall_counter,
            ally_switch_counter: mon.ally_switch_counter,
            possible_natures: Unknown::Known(mon.nature),
            minEvs: mon.evs,
            maxEvs: mon.evs,
            minIvs: mon.ivs,
            maxIvs: mon.ivs,
            minStats: mon.stats,
            maxStats: mon.stats,
            // Pre-nature BSV bounds.
            // For own (fully-known) Pokémon, PokemonState does not store base stats, so we
            // cannot compute BSV exactly without the dex. Since EVIVStatGE/LE predicates are
            // only ever emitted for *opponent* mons, these bounds on own mons are never used
            // by BCP or Pass 3 to constrain anything — we set a maximally wide range that is
            // vacuously sound.  If tighter own-mon BSV bounds are later needed, thread the
            // dex through here and compute calc_stat(base, iv, ev, level, 1.0).
            min_pre_nature_stat: [0u16; 6],
            max_pre_nature_stat: [u16::MAX; 6],
            boosts: mon.boosts,
            status: mon.status.clone(),
            volatiles: mon.volatiles.clone(),
            possible_original_abilities: Unknown::Known(
                mon.original_ability
                    .clone()
                    .unwrap_or_else(|| mon.ability.clone()),
            ),
            possible_abilities: Unknown::Known(mon.ability.clone()),
            possible_genders: Unknown::Known(mon.gender),
            possible_weight_hg: Unknown::Known(mon.weight_hg),
            possible_tera_type: Unknown::Known(mon.tera_type.clone()),
            mega_species: Unknown::Known(mon.mega_species.clone()),
            mega_ability: Unknown::Known(mon.mega_ability.clone()),
            last_move_failed: mon.last_move_failed,
            last_used_move: mon.last_used_move.clone(),
            consecutive_move_count: mon.consecutive_move_count,
            used_moves_this_field: mon.used_moves_this_field,
            one_time_ability_used: mon.one_time_ability_used,
            ate_berry_this_battle: mon.ate_berry_this_battle,
            first_move_on_field: mon.first_move_on_field,
            first_turn_on_field_pending: mon.first_turn_on_field_pending,
            entered_this_turn: mon.entered_this_turn,
            pre_transform: mon
                .pre_transform
                .as_deref()
                .map(|p| Box::new(UnknownPokemonState::from_known_pokemon(p))),
            pre_mimicry_types: mon.pre_mimicry_types.clone(),
            times_hit: mon.times_hit,
        }
    }

    /// Build a species-only fog-of-war view of an **opponent's** Pokémon as seen at team
    /// preview.
    ///
    /// Fields that are 1:1 with species (types, weight) are set to `Known`; everything
    /// else is fully unknown (`Not(vec![])`).  Stat bounds are computed as the theoretical
    /// worst-case (0 IVs / 0 EVs / hindering nature) and best-case (31 IVs / 252 EVs /
    /// boosting nature) for each stat independently.
    pub fn from_opponent_species(
        species: Species,
        dex: &HashMap<Species, PokemonData>,
        level: u8,
    ) -> Self {
        let data = dex.get(&species);
        let types = data
            .map(|d| d.types.clone())
            .unwrap_or_else(|| vec![PokemonType::Normal]);
        let weight_hg = data.map(|d| d.weight).unwrap_or(0);
        let base = data.map(|d| d.base_stats).unwrap_or([100u16; 6]);
        let default_gender = data
            .map(|d| d.default_gender)
            .unwrap_or(PokemonGender::Genderless);

        // Independent per-stat min/max: natures boost one stat ×1.1 and hinder another ×0.9,
        // so each stat's range is calculated separately rather than using a single nature.
        let min_stats: PokemonStatsTable = [
            calc_hp(base[0], 0, 0, level),
            calc_stat(base[1], 0, 0, level, 0.9),
            calc_stat(base[2], 0, 0, level, 0.9),
            calc_stat(base[3], 0, 0, level, 0.9),
            calc_stat(base[4], 0, 0, level, 0.9),
            calc_stat(base[5], 0, 0, level, 0.9),
        ];
        let max_stats: PokemonStatsTable = [
            calc_hp(base[0], 31, 252, level),
            calc_stat(base[1], 31, 252, level, 1.1),
            calc_stat(base[2], 31, 252, level, 1.1),
            calc_stat(base[3], 31, 252, level, 1.1),
            calc_stat(base[4], 31, 252, level, 1.1),
            calc_stat(base[5], 31, 252, level, 1.1),
        ];
        // Pre-nature BSV bounds (nature-independent: calc_stat with mod=1.0).
        // HP (index 0) has no nature modifier, so BSV == final stat.
        // For non-HP stats, BSV range is wider than the post-nature range because nature is
        // stripped: the minimum BSV (0 EV/IV, no nature boost) and maximum BSV (31 IV/252 EV,
        // no nature nerf) span the achievable pre-nature lattice.
        let min_pre_nature: PokemonStatsTable = [
            calc_hp(base[0], 0, 0, level),
            calc_stat(base[1], 0, 0, level, 1.0),
            calc_stat(base[2], 0, 0, level, 1.0),
            calc_stat(base[3], 0, 0, level, 1.0),
            calc_stat(base[4], 0, 0, level, 1.0),
            calc_stat(base[5], 0, 0, level, 1.0),
        ];
        let max_pre_nature: PokemonStatsTable = [
            calc_hp(base[0], 31, 252, level),
            calc_stat(base[1], 31, 252, level, 1.0),
            calc_stat(base[2], 31, 252, level, 1.0),
            calc_stat(base[3], 31, 252, level, 1.0),
            calc_stat(base[4], 31, 252, level, 1.0),
            calc_stat(base[5], 31, 252, level, 1.0),
        ];

        // Genderless species are always genderless; sexed species gender is unknown.
        let possible_genders = if default_gender == PokemonGender::Genderless {
            Unknown::Known(PokemonGender::Genderless)
        } else {
            Unknown::Not(Vec::new())
        };

        UnknownPokemonState {
            possible_mon_id: Unknown::Not(Vec::new()),
            fainted: false,
            possible_species: Unknown::Known(species),
            possible_types: Unknown::Known(types),
            is_tera: false,
            is_mega: false,
            level,
            hp: PokemonHP::Percent(100),
            known_moves: [None, None, None, None],
            move_pp: [-1; 4],
            max_pp: [-1; 4],
            item: Unknown::Not(Vec::new()),
            consumed_item: None,
            cud_chew_pending: None,
            item_lost: false,
            removed_item: None,
            damaged_this_turn: false,
            damaged_by_this_turn: Vec::new(),
            last_physical_damage_taken: PokemonHP::Percent(0),
            last_physical_attacker: None,
            last_special_damage_taken: PokemonHP::Percent(0),
            last_special_attacker: None,
            last_damage_taken: PokemonHP::Percent(0),
            last_damage_attacker: None,
            stats_raised_this_turn: false,
            stats_lowered_this_turn: false,
            switched_in_this_turn: false,
            stall_counter: 0,
            ally_switch_counter: 0,
            possible_natures: Unknown::Not(Vec::new()),
            minEvs: [0; 6],
            maxEvs: [252; 6],
            minIvs: [0; 6],
            maxIvs: [31; 6],
            minStats: min_stats,
            maxStats: max_stats,
            min_pre_nature_stat: min_pre_nature,
            max_pre_nature_stat: max_pre_nature,
            boosts: [0; 7],
            status: None,
            volatiles: Vec::new(),
            possible_original_abilities: if data.map_or(false, |d| !d.abilities.is_empty()) {
                Unknown::Possibly(data.unwrap().abilities.clone())
            } else {
                Unknown::Not(Vec::new())
            },
            possible_abilities: if data.map_or(false, |d| !d.abilities.is_empty()) {
                Unknown::Possibly(data.unwrap().abilities.clone())
            } else {
                Unknown::Not(Vec::new())
            },
            possible_genders,
            possible_weight_hg: Unknown::Known(weight_hg),
            possible_tera_type: Unknown::Not(Vec::new()),
            mega_species: Unknown::Not(Vec::new()),
            mega_ability: Unknown::Not(Vec::new()),
            last_move_failed: false,
            last_used_move: None,
            consecutive_move_count: 0,
            used_moves_this_field: [false; 4],
            one_time_ability_used: false,
            ate_berry_this_battle: false,
            first_move_on_field: false,
            first_turn_on_field_pending: false,
            entered_this_turn: false,
            pre_transform: None,
            pre_mimicry_types: None,
            times_hit: 0,
        }
    }
}

// ── UnknownMatchState constructors ────────────────────────────────────────────

impl UnknownMatchState {
    /// Build a team-preview fog-of-war state from the perspective of `viewer`.
    ///
    /// * `my_team` — the viewer's own Pokémon, fully parsed from the teamsheet.
    /// * `opponent_species` — the 6 species shown at team preview for the other side.
    ///
    /// The viewer's side is fully known; the opponent's side carries only species-derived
    /// information.  `viewer = P1` places `my_team` into `p1_mons` and the opponent into
    /// `p2_mons`; `viewer = P2` reverses the assignment.
    pub fn team_preview_from_perspective(
        viewer: Player,
        my_team: &[PokemonState],
        opponent_species: &[Species],
        dex: &HashMap<Species, PokemonData>,
        active_per_side: u8,
        brought_per_side: u8,
        level: u8,
    ) -> UnknownMatchState {
        let my_mons: Vec<UnknownPokemonState> = my_team
            .iter()
            .map(UnknownPokemonState::from_known_pokemon)
            .collect();
        let opp_mons: Vec<UnknownPokemonState> = opponent_species
            .iter()
            .map(|s| UnknownPokemonState::from_opponent_species(s.clone(), dex, level))
            .collect();

        let (p1_mons, p2_mons) = match viewer {
            Player::P1 => (my_mons, opp_mons),
            Player::P2 => (opp_mons, my_mons),
        };

        UnknownMatchState::TeamPreview(UnknownTeamPreviewState {
            active_per_side,
            brought_per_side,
            p1_mons,
            p2_mons,
        })
    }
}
