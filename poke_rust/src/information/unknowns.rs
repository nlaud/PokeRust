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

/// Selects the starting fog-of-war baseline for an opponent's Pokémon at team preview.
/// `PerfectInformation` means no belief is tracked at all (the server keeps `belief =
/// None` and ships ground truth) — this variant exists for completeness/testing only;
/// `from_opponent_open_sheet` is never actually invoked with it in production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InformationMode {
    PerfectInformation,
    /// Only the opponent's species are visible at team preview (the traditional VGC/
    /// Champions competitive format); moves/item/ability/nature/EVs/IVs/Tera type all
    /// stay fully unknown until revealed through play.
    ClosedTeamSheet,
    /// Species/ability/item/moves/Tera type revealed up front (like a real VGC open
    /// team sheet); nature/EVs/IVs/exact stats still hidden.
    OpenTeamSheet,
    /// `OpenTeamSheet` plus the Pokémon's nature.
    OpenTeamSheetNatures,
}

#[derive(Debug, Clone)]
pub struct UnknownPokemonState {
    pub possible_mon_id: Unknown<u8>, //1:1 with species if there is only 1 of each pokemon
    pub fainted: bool,
    pub possible_species: Unknown<Species>,
    pub possible_types: Unknown<Vec<PokemonType>>, //These two fields correspond 1:1
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
    /// True once this Pokémon's current held item arrived via a mid-battle transfer
    /// (Trick, Switcheroo, Symbiosis, Recycle, Pickup — anything emitted as
    /// `ItemGained`) rather than being its own team-built item. Gates the item-clause
    /// exclusion (`enforce_unique_item`): a transferred item is not evidence about what
    /// this Pokémon's OWN team built, so a later `ItemRevealed` re-confirming it must
    /// not exclude that item from this mon's teammates. Persists across switches
    /// (transfers are permanent for the rest of the battle) — only cleared by a further
    /// `ItemGained`/`ItemLost` cycle back to a state where the flag is no longer needed
    /// to reason about (kept `true` once set; see S12 in the inference audit).
    pub item_was_transferred: bool,

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
    pub min_evs: [u8; 6],
    pub max_evs: [u8; 6],
    pub min_ivs: [u8; 6],
    pub max_ivs: [u8; 6],

    pub min_stats: PokemonStatsTable,
    pub max_stats: PokemonStatsTable,

    /// Pre-nature base stat value bounds: `calc_stat(base, iv, ev, level, 1.0)` before ×0.9/1.0/1.1.
    /// Tightened by Pass 3 (damage→stat inversion). Used by Pass 5 + Pass 6 `EVIVStatGE`/`EVIVStatLE`.
    /// Index matches `min_stats`/`max_stats`: 0=HP, 1=Atk, 2=Def, 3=SpA, 4=SpD, 5=Spe.
    pub min_pre_nature_stat: PokemonStatsTable,
    pub max_pre_nature_stat: PokemonStatsTable,

    pub boosts: PokemonBoostTable,
    pub status: Option<Status>,

    pub volatiles: Vec<VolatileStatusState>, //If this might be unknown, replace it with Unknown<Vec<VolatileStatusState>>

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
    pub pre_transform: Option<Box<UnknownPokemonState>>,

    /// Original types saved when Mimicry overwrites them. Restored when terrain ends or
    /// the holder switches out. `None` when Mimicry is not active.
    pub pre_mimicry_types: Option<Vec<crate::state::dex_data::PokemonType>>,

    /// Hit counter for Rage Fist. Incremented each time this Pokémon is hit by a direct damaging
    /// move (from any source, including allies). Reset on switch-out or faint (Champions rules);
    /// never reset at end-of-turn while the mon remains on field.
    pub times_hit: u16,

    /// Zoroark/Illusion parallel hypothesis: "the restrictions on this physical mon IF it is
    /// actually a disguised Illusion-forme (Zoroark line)". `None` when this mon cannot be an
    /// unrevealed Illusion user (Illusion already resolved, no Illusion mon possible on this
    /// side, or this mon's shown species already IS the Illusion forme). When `Some`, every
    /// inference pass that constrains this mon (Pass 1 reveals, Pass 2 presence/absence, Pass 3
    /// damage→stats, Pass 4 speed, Pass 5 back-solve, Pass 6 BCP) is mirrored onto the boxed
    /// sub-state under the assumption "this physical slot is the Zoroark", via
    /// `apply_with_illusion_mirroring` (`information::inference`):
    ///
    ///   - primary OK, sub-state contradicts  ⇒ not Zoroark, drop this field (`None`).
    ///   - primary contradicts, sub-state OK  ⇒ IS Zoroark, promote the sub-state to replace
    ///     this mon's own fields (see `promote_illusion_to_primary`), then clear this field.
    ///   - both contradict                    ⇒ genuine impossibility (panics as usual).
    ///
    /// Conditional constraints accumulated here are valid ONLY under "this physical mon is the
    /// Zoroark" and must never leak to a different physical mon (see the isolation rule in
    /// `information::inference`'s Zoroark lifecycle docs): a switch-out retains this field
    /// (bench pairs it with the same physical entry), and a switch-in only ever seeds a BRAND
    /// NEW mon's copy from the side's unconditional Zoroark baseline, never from another slot's
    /// accumulated sub-state. Always `None` on the sub-state itself (no nesting — a disguised
    /// Zoroark's hypothetical self cannot itself be hypothetically disguised).
    pub possible_illusion_state: Option<Box<UnknownPokemonState>>,
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
    /// Opponent mons that fainted and were then replaced (the outgoing entry at
    /// the moment of replacement — see `bench_outgoing_mon`). Deliberately kept
    /// OUTSIDE the `mon_idx` flat-index space (the 6 segments enumerated in
    /// `get_mon_by_idx`/`get_mon_mut_by_idx`/`mon_idx_legend`): a fainted mon has
    /// its scoped predicates purged on switch-out, so nothing ever references it
    /// by `mon_idx` again, and it must also be excluded from "could this be a
    /// hidden back mon" reasoning (`combined_back` and friends). This bucket
    /// exists purely so the belief retains the knowledge accumulated about the
    /// mon (species, revealed moves/item/ability) for display — a fainted-and-
    /// replaced opponent used to be silently discarded with no record it ever
    /// existed.
    pub p1_fainted_mons: Vec<UnknownPokemonState>,
    pub p2_fainted_mons: Vec<UnknownPokemonState>,

    /// Number of this side's REAL roster members that are an Illusion-capable
    /// forme (Zoroark line) whose physical identity/location has not yet been
    /// positively pinned down. Almost always 0 or 1 (Species Clause permits at
    /// most one). Seeded once, at the team-preview→battle transition, from the
    /// true roster (`into_battle_state`) — team preview always reveals true
    /// species, so this is never itself uncertain. While `> 0`, every OTHER mon
    /// on this side that isn't itself confirmed to BE that Illusion forme may
    /// carry a `possible_illusion_state` hypothesis (see that field's doc
    /// comment on `UnknownPokemonState`). Decremented by `resolve_zoroark_globally`
    /// each time a hypothesis is positively resolved (promotion, `IllusionEnded`,
    /// or the Illusion forme itself entering undisguised); at 0, every remaining
    /// `possible_illusion_state` on the side is dropped — Zoroark's location(s)
    /// are now fully accounted for.
    pub p1_unresolved_zoroark_count: u8,
    pub p2_unresolved_zoroark_count: u8,

    /// Pristine "what team preview told us about species S" snapshot for every
    /// physical roster member on this side, captured ONCE at the team-preview→battle
    /// transition (`into_battle_state`), before any switch/disguise churn touches it.
    /// Under an open team sheet these entries carry the full `Known` item/moves/
    /// ability/nature set; under species-only preview they're identical to what
    /// `from_opponent_species` would already build.
    ///
    /// Exists purely as a restore source: `restore_discarded_primary_to_bench` (after
    /// an `IllusionEnded` promotion) and `pass1_switch`'s "species not found on the
    /// bench" fallback both need to rebuild a roster entry from scratch, and MUST
    /// prefer cloning from here over calling `from_opponent_species` — the latter is
    /// species-only and, under an open sheet, would regress a fully-known mon back to
    /// "no information" the moment it's rebuilt (see the TODO.md Zoroark-switching
    /// regression this fixes). Never mutated after `into_battle_state` populates it;
    /// never itself displayed or read by any other pass.
    pub p1_roster_templates: Vec<UnknownPokemonState>,
    pub p2_roster_templates: Vec<UnknownPokemonState>,

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
    /// `mon_idx` of the Pokémon that last set the current weather (via move or on-switch ability).
    /// `None` when the setter is unknown or when no weather is active.
    /// Used by I-A: when the timer collapses from `Possibly([5,8])` to `Known(3)`,
    /// we know the setter had the corresponding rock item and emit `HasItem` as `Known`.
    pub weather_setter_mon_idx: Option<usize>,
    pub pseudo_weathers: Vec<PseudoWeather>,
    pub pseudo_weather_turns: Vec<Unknown<u8>>,
    pub terrain: Option<Terrain>,
    pub terrain_turns: Option<Unknown<u8>>,
    /// `mon_idx` of the Pokémon that last set the current terrain.  Same purpose as
    /// `weather_setter_mon_idx`: reveals `TerrainExtender` when the timer collapses.
    pub terrain_setter_mon_idx: Option<usize>,
    pub p1_side_conditions: Vec<SideCondition>,
    pub p1_side_condition_turns: Vec<Unknown<u8>>,
    /// Parallel to `p1_side_conditions`/`p1_side_condition_turns`.
    /// `mon_idx` of the setter for each active P1 side condition; `None` if unknown.
    /// Used to reveal `LightClay` when a screen timer collapses from `Possibly([5,8])` to `Known(3)`.
    pub p1_side_condition_setters: Vec<Option<usize>>,
    pub p2_side_conditions: Vec<SideCondition>,
    pub p2_side_condition_turns: Vec<Unknown<u8>>,
    /// Parallel to `p2_side_conditions`/`p2_side_condition_turns`.
    pub p2_side_condition_setters: Vec<Option<usize>>,
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
/// [p1_active_mons..., p2_active_mons...,
///  p1_known_back_mons..., p1_possible_back_mons...,
///  p2_known_back_mons..., p2_possible_back_mons...]
/// ```
///
/// Both active segments come first, before either side's bench (S1: this keeps
/// every active mon's `mon_idx` stable for the whole battle — see the
/// `mon_idx` helpers doc comment in `information::inference` for why the naive
/// per-side-contiguous ordering was unsound for persistent `Statement`s). A
/// side's full roster (active + bench) is therefore NOT one contiguous range —
/// `teammate_indices` / `mon_is_p2` in `information::inference` check each
/// segment explicitly.
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
    HasAbility {
        mon_idx: usize,
        ability: Ability,
    },
    /// The active weather lasts exactly `turns` more end-of-turns. Emitted as a clause
    /// pair tying the setter's extension rock to the duration; the `turns` payload is
    /// decremented in sync with the field timer each end-of-turn.
    WeatherTurns {
        turns: usize,
    },
    /// The active terrain lasts exactly `turns` more end-of-turns (Terrain Extender pair).
    TerrainTurns {
        turns: usize,
    },
    /// The given side condition lasts exactly `turns` more end-of-turns (Light Clay pair).
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
    /// Cross-mon effective-speed comparison in the same priority bracket:
    /// `base_spe(fast_idx) * fast_mult >= base_spe(slow_idx) * slow_mult`, where the
    /// mults are the product of all *observable* speed modifiers at observation time
    /// (Trick Room already handled by swapping fast/slow). Hidden-item or random-source
    /// explanations (Iron Ball, Quick Claw, etc.) are emitted as separate disjunctive
    /// clauses, not folded into these multipliers.
    SpeedComparison {
        fast_idx: usize,
        slow_idx: usize,
        fast_mult: u32,
        slow_mult: u32,
    },
    /// A persistent relational constraint emitted by Anticipation:
    /// the Pokémon at `mon_idx` knows at least one move that is super-effective
    /// against `defender_types`, or knows an OHKO move (Fissure, Guillotine,
    /// Horn Drill, Sheer Cold).
    ///
    /// Like `SpeedComparison`, this is never unit-propagated into a concrete field;
    /// BCP only *satisfies* (drops) or *prunes* (removes literal) this clause.
    KnowsThreateningMove {
        mon_idx: usize,
        defender_types: Vec<PokemonType>,
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

impl UnknownTeamPreviewState {
    /// Convert the team-preview fog state into the battle-phase `UnknownBattleState`,
    /// once both players' team-preview lead/bring selections are known. Mirrors the
    /// concrete `battle_state_from_preview_branching` (`simulator/mod.rs`), which
    /// already receives exactly these four index arrays via `TeamPreviewCommand`.
    ///
    /// Fog mons are matched to their roster bucket by **original list position**
    /// (not `mon_id` — an opponent's `possible_mon_id` is still unresolved at preview
    /// time), using the same indices the concrete transition used, so belief and
    /// ground truth stay in lockstep.
    ///
    /// `active_indices` and `back_indices` together only cover `brought_per_side`
    /// mons. In a bring-N-of-M format, any opponent-side entry whose index appears in
    /// neither list was shown at team preview but never brought into this battle —
    /// those go to that side's `possible_back_mons` at the bare species-only baseline
    /// (no moves/item/ability/nature reveal, regardless of information mode: a mon
    /// that's not in this battle can never affect it, so there is nothing more to
    /// know). The viewer's own side has no such gap — it's always fully known,
    /// brought or not — so its `possible_back_mons` is always empty.
    ///
    /// `self.p1_mons`/`self.p2_mons` are **physically bound**: `p1_mons` always holds
    /// physical P1's team, `p2_mons` always holds physical P2's team, at whichever fog
    /// level `viewer` implies (see `UnknownMatchState::team_preview_open_sheet_from_perspective`'s
    /// tuple destructuring — the `my_team`/`opponent_team` fog levels land in the
    /// PHYSICALLY correct bucket, not a "viewer's own" bucket). `viewer` here decides
    /// which physical side gets the direct known-active treatment (its own picks,
    /// `p{viewer}_active_indices`/`p{viewer}_back_indices`) and which gets the
    /// whole-roster-to-`possible_back` treatment — the SAME split for whichever
    /// belief this seeds, just applied to the opposite physical side when viewer=P2.
    pub fn into_battle_state(
        &self,
        viewer: Player,
        p1_active_indices: &[usize],
        p1_back_indices: &[usize],
        p2_active_indices: &[usize],
        p2_back_indices: &[usize],
    ) -> UnknownBattleState {
        let pick = |mons: &[UnknownPokemonState], indices: &[usize]| -> Vec<UnknownPokemonState> {
            indices.iter().filter_map(|&i| mons.get(i).cloned()).collect()
        };

        // The non-viewer side's ENTIRE roster — including whatever it selected as
        // active — starts in `possible_back`, and its `active_mons` starts empty.
        // This is deliberate, not an oversight: unlike the viewer's own side (always
        // fully known), the viewer never learns WHICH physical roster slot the
        // opponent truly leads with — only what's DISPLAYED once it's sent out,
        // which can be an Illusion disguise. Directly copying the true active index
        // (the old behavior) used ground truth the belief has no business knowing —
        // so a leading, disguised Zoroark was built as `Known(Zoroark)` with its full
        // real stats/moves/item from turn 0, and the "possibly in the back" entry for
        // that same physical mon never got created.
        //
        // Instead, the caller (`session.rs::resolve_turn`) is now expected to run
        // `apply_information` over the team-preview transition's own event log
        // immediately after this call, before storing the result as the belief. That
        // log's `SimultaneousSwitch` already carries each lead's DISPLAYED species,
        // perspective-gated by `compute_illusion_disguise`
        // (`simulator/mod.rs::battle_state_from_preview_branching`) as the real game
        // would show it — so Pass 1's existing switch-in handling (`pass1_switch`,
        // shared verbatim with every mid-battle switch) matches it against this same
        // `possible_back` roster by species and pulls the correct entry into the
        // active slot, running the SAME Illusion-widening
        // (`maybe_widen_for_illusion`/`widen_item_for_illusion`) a mid-battle
        // disguised switch-in already gets. For a normal (non-disguised) lead this
        // produces byte-identical results to the old direct-copy — the match is
        // exact and the pulled entry carries the same open-sheet data — but for a
        // genuinely disguised lead it correctly leaves the belief ambiguous.
        //
        // `known_back` must only ever hold mons that have been battle-confirmed by
        // being active and then withdrawn — exactly what `bench_outgoing_mon` does
        // mid-battle, and what `pass1_switch`'s known-then-possible fallback already
        // expects to pull a first-time switch-in from. Dumping any indices straight
        // into `known_back` here bypasses that and makes a bench mon "immediately
        // show up" as already-revealed at turn 0.
        let (p1_active_mons, p1_known_back_mons, p1_possible_back_mons) = if viewer == Player::P1 {
            (pick(&self.p1_mons, p1_active_indices), pick(&self.p1_mons, p1_back_indices), Vec::new())
        } else {
            (Vec::new(), Vec::new(), self.p1_mons.clone())
        };
        let (p2_active_mons, p2_known_back_mons, p2_possible_back_mons) = if viewer == Player::P2 {
            (pick(&self.p2_mons, p2_active_indices), pick(&self.p2_mons, p2_back_indices), Vec::new())
        } else {
            (Vec::new(), Vec::new(), self.p2_mons.clone())
        };

        let total_roster = self.p1_mons.len().max(self.p2_mons.len()) as u8;

        let mut result = UnknownBattleState {
            active_per_side: self.active_per_side,
            back_mons_per_side: total_roster.saturating_sub(self.active_per_side),

            p1_active_mons,
            p2_active_mons,
            p1_known_back_mons,
            p2_known_back_mons,
            p1_possible_back_mons,
            p2_possible_back_mons,
            p1_fainted_mons: Vec::new(),
            p2_fainted_mons: Vec::new(),

            // Computed below by `seed_illusion_hypotheses`, which scans each side's
            // freshly-populated `possible_back` roster for Illusion-capable formes.
            p1_unresolved_zoroark_count: 0,
            p2_unresolved_zoroark_count: 0,

            // Pristine snapshot of what team preview told us, per physical roster
            // member, BEFORE the viewer-side/opponent-side split above and any later
            // switch/disguise churn — see the field's doc comment. Cloned from
            // `self.p1_mons`/`self.p2_mons` directly (not from whichever bucket this
            // physical side ended up in), since `possible_illusion_state` is `None` on
            // every entry at this point regardless (seeding happens below).
            p1_roster_templates: self.p1_mons.clone(),
            p2_roster_templates: self.p2_mons.clone(),

            turn_number: 0,
            turn_started: false,
            turn_ended: false,

            p1_has_tera: true,
            p2_has_tera: true,
            p1_has_mega: true,
            p2_has_mega: true,

            weather: None,
            weather_turns: None,
            weather_setter_mon_idx: None,
            pseudo_weathers: Vec::new(),
            pseudo_weather_turns: Vec::new(),
            terrain: None,
            terrain_turns: None,
            terrain_setter_mon_idx: None,
            p1_side_conditions: Vec::new(),
            p1_side_condition_turns: Vec::new(),
            p1_side_condition_setters: Vec::new(),
            p2_side_conditions: Vec::new(),
            p2_side_condition_turns: Vec::new(),
            p2_side_condition_setters: Vec::new(),
            p1_slot_conditions: vec![Vec::new(); self.active_per_side as usize],
            p2_slot_conditions: vec![Vec::new(); self.active_per_side as usize],

            self_switch_pending: None,
            items_consumed_this_turn: Vec::new(),
            last_move_on_field: None,
            sub_damage_dealt: 0,
            round_used_this_turn: false,

            predicates: Vec::new(),
        };

        // Seed the Zoroark parallel-hypothesis on whichever side(s) just got dumped
        // into `possible_back` wholesale (see the big comment above) — the viewer's
        // own side's `possible_back` is always empty at this point (its mons are
        // fully known and placed directly into active/known_back instead), so this
        // is a harmless no-op there: a player always knows their own team's true
        // identity and never carries a hypothesis about themselves.
        seed_illusion_hypotheses(&mut result, Player::P1);
        seed_illusion_hypotheses(&mut result, Player::P2);

        result
    }
}

/// Species capable of the Illusion ability (used to disguise as another party
/// member) — the whole Zorua/Zoroark line, both regional formes. Team preview
/// always reveals TRUE species (Illusion only disguises appearance once a
/// Pokémon is actually sent out mid-battle, never at preview), so membership in
/// this set is never itself uncertain — it's exactly what lets the engine know,
/// from turn 0, which physical roster slot the sub-state hypotheses are for.
pub(crate) fn is_illusion_capable_species(species: &Species) -> bool {
    matches!(
        species,
        Species::Zorua | Species::ZoruaHisui | Species::Zoroark | Species::ZoroarkHisui
    )
}

/// Seed `possible_illusion_state` on every non-Illusion-forme entry in `side`'s
/// freshly-populated `possible_back` roster, and set
/// `p{side}_unresolved_zoroark_count` to the number of real Illusion-forme
/// roster members found. No-op if the side has none (the common case) or if
/// `possible_back` is empty (the viewer's own side).
///
/// Called exactly once, at the team-preview→battle transition
/// (`into_battle_state`) — never again — because that is the only moment the
/// WHOLE roster is dumped in at once as freshly-constructed entries with no
/// battle history yet. Every later appearance of one of these physical mons
/// (switching in, switching back out) carries its hypothesis along for free,
/// since bench bookkeeping (`pass1_switch`/`bench_outgoing_mon` in
/// `information::inference`) moves/clones the WHOLE `UnknownPokemonState` —
/// nested `possible_illusion_state` included — rather than rebuilding entries
/// from scratch.
fn seed_illusion_hypotheses(state: &mut UnknownBattleState, side: Player) {
    let roster: &[UnknownPokemonState] = match side {
        Player::P1 => &state.p1_possible_back_mons,
        Player::P2 => &state.p2_possible_back_mons,
    };

    let is_illusion_entry = |m: &UnknownPokemonState| {
        matches!(&m.possible_species, Unknown::Known(s) if is_illusion_capable_species(s))
    };

    let count = roster.iter().filter(|m| is_illusion_entry(m)).count() as u8;
    match side {
        Player::P1 => state.p1_unresolved_zoroark_count = count,
        Player::P2 => state.p2_unresolved_zoroark_count = count,
    }
    if count == 0 {
        return;
    }

    // The template used to seed every OTHER mon's hypothesis. With Species Clause
    // (at most one Illusion forme per team) this is unambiguous; if a format ever
    // allowed more than one, the first is used as the template for all of them —
    // a sound simplification (see the plan's multi-Zoroark note), not a precision
    // loss any current format actually exercises.
    let baseline = roster.iter().find(|m| is_illusion_entry(m)).cloned().expect(
        "count > 0 implies at least one Illusion-forme entry exists in this roster",
    );

    let roster_mut: &mut Vec<UnknownPokemonState> = match side {
        Player::P1 => &mut state.p1_possible_back_mons,
        Player::P2 => &mut state.p2_possible_back_mons,
    };
    for mon in roster_mut.iter_mut() {
        if !is_illusion_entry(mon) {
            mon.possible_illusion_state =
                Some(Box::new(seed_illusion_hypothesis_for(mon, &baseline)));
        }
    }
}

/// Build a fresh Zoroark hypothesis for `host` (a specific physical roster slot)
/// from `baseline` (the side's real Illusion-forme roster entry): "the
/// restrictions on this physical mon IF it is actually Zoroark, disguised as
/// whatever `host`'s own species is."
///
/// Takes every IDENTITY field (species, types, ability, moves, item, nature,
/// stat/EV/IV bounds, tera type, mega data, party-order id, …) from `baseline`
/// — that's what's in question. Overwrites every PHYSICALLY-OBSERVABLE, per-slot
/// field (HP, level, status, boosts, volatiles, fainted, is_tera/is_mega, every
/// per-turn/once-per-battle flag, `times_hit`, …) with `host`'s own current
/// value: those describe facts about THIS PHYSICAL MON as directly observed so
/// far, true regardless of which identity hypothesis turns out to be correct —
/// `baseline`'s own copies of those fields describe a DIFFERENT physical mon
/// (wherever the real Zoroark's own tenure has taken it) and must never leak in
/// (see the isolation rule in `information::inference`'s Zoroark lifecycle docs).
/// Adding a new field to `UnknownPokemonState` later requires deciding which of
/// these two categories it falls into and updating this function accordingly.
pub(crate) fn seed_illusion_hypothesis_for(
    host: &UnknownPokemonState,
    baseline: &UnknownPokemonState,
) -> UnknownPokemonState {
    let mut sub = baseline.clone();
    sub.fainted = host.fainted;
    sub.level = host.level;
    sub.hp = host.hp.clone();
    sub.boosts = host.boosts;
    sub.status = host.status.clone();
    sub.volatiles = host.volatiles.clone();
    sub.is_tera = host.is_tera;
    sub.is_mega = host.is_mega;
    sub.damaged_this_turn = host.damaged_this_turn;
    sub.damaged_by_this_turn = host.damaged_by_this_turn.clone();
    sub.last_physical_damage_taken = host.last_physical_damage_taken.clone();
    sub.last_physical_attacker = host.last_physical_attacker;
    sub.last_special_damage_taken = host.last_special_damage_taken.clone();
    sub.last_special_attacker = host.last_special_attacker;
    sub.last_damage_taken = host.last_damage_taken.clone();
    sub.last_damage_attacker = host.last_damage_attacker;
    sub.stats_raised_this_turn = host.stats_raised_this_turn;
    sub.stats_lowered_this_turn = host.stats_lowered_this_turn;
    sub.switched_in_this_turn = host.switched_in_this_turn;
    sub.stall_counter = host.stall_counter;
    sub.ally_switch_counter = host.ally_switch_counter;
    sub.last_move_failed = host.last_move_failed;
    sub.last_used_move = host.last_used_move.clone();
    sub.consecutive_move_count = host.consecutive_move_count;
    sub.used_moves_this_field = host.used_moves_this_field;
    sub.one_time_ability_used = host.one_time_ability_used;
    sub.ate_berry_this_battle = host.ate_berry_this_battle;
    sub.first_move_on_field = host.first_move_on_field;
    sub.first_turn_on_field_pending = host.first_turn_on_field_pending;
    sub.entered_this_turn = host.entered_this_turn;
    sub.pre_transform = host.pre_transform.clone();
    sub.pre_mimicry_types = host.pre_mimicry_types.clone();
    sub.times_hit = host.times_hit;
    sub.possible_illusion_state = None; // no nesting — see the field's doc comment
    sub
}

/// Fog-of-war analogue of `battle::MatchState`.  Tracks the game phase from a single
/// player's perspective so observation code can pattern-match symmetrically with the
/// concrete state machine.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
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
            item_was_transferred: false,
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
            min_evs: mon.evs,
            max_evs: mon.evs,
            min_ivs: mon.ivs,
            max_ivs: mon.ivs,
            min_stats: mon.stats,
            max_stats: mon.stats,
            // EVIVStatGE/LE predicates are only ever emitted for *opponent* mons, so these
            // bounds are never used to constrain own mons — a maximally wide range is fine.
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
            // Your own mon's true identity is never in question.
            possible_illusion_state: None,
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
        // Pre-nature BSV bounds (calc_stat with mod=1.0). HP has no nature modifier so
        // BSV == final stat; other stats get a wider range since nature scaling is stripped.
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
            item_was_transferred: false,
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
            min_evs: [0; 6],
            max_evs: [252; 6],
            min_ivs: [0; 6],
            max_ivs: [31; 6],
            min_stats,
            max_stats,
            min_pre_nature_stat: min_pre_nature,
            max_pre_nature_stat: max_pre_nature,
            boosts: [0; 7],
            status: None,
            volatiles: Vec::new(),
            possible_original_abilities: if data.is_some_and(|d| !d.abilities.is_empty()) {
                Unknown::Possibly(data.unwrap().abilities.clone())
            } else {
                Unknown::Not(Vec::new())
            },
            possible_abilities: if data.is_some_and(|d| !d.abilities.is_empty()) {
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
            // Seeded separately by the Zoroark-lifecycle logic in `information::inference`
            // (which knows whether this side has an unresolved Illusion mon and whether this
            // physical slot is eligible) — never set here, since this constructor has no
            // visibility into the rest of the roster.
            possible_illusion_state: None,
        }
    }
}

impl UnknownPokemonState {
    /// Tighten the min-side IV/stat bounds to reflect a format that pins opponent IVs
    /// to 31 (Pokémon Champions competitive default, `InferenceConfig::force_max_ivs`).
    ///
    /// `from_opponent_species` always seeds the full `[0, 31]` IV lattice since it has
    /// no visibility into the format's IV-pinning rule. When the format pins IVs, the
    /// min-side stat/BSV bounds must be recomputed at IV 31, not IV 0 — otherwise the
    /// stored window spans a "phantom" region (IV-0-achievable but never
    /// IV-31-achievable) that `pass3_direction_b`'s damage back-solve can narrow into,
    /// while `pass5_back_solve` (which correctly restricts its own search to IV 31 per
    /// this same config flag) can never satisfy — producing "every candidate nature is
    /// infeasible" contradictions on ordinary turns. See the `test_s34_*` regression
    /// tests below. Shared by `from_opponent_open_sheet` and
    /// `team_preview_closed_sheet_from_perspective`.
    fn pin_min_ivs_to_max(&mut self, species: &Species, dex: &HashMap<Species, PokemonData>, level: u8) {
        self.min_ivs = [31; 6];
        if let Some(data) = dex.get(species) {
            let b = data.base_stats;
            self.min_stats = [
                calc_hp(b[0], 31, 0, level),
                calc_stat(b[1], 31, 0, level, 0.9),
                calc_stat(b[2], 31, 0, level, 0.9),
                calc_stat(b[3], 31, 0, level, 0.9),
                calc_stat(b[4], 31, 0, level, 0.9),
                calc_stat(b[5], 31, 0, level, 0.9),
            ];
            self.min_pre_nature_stat = [
                calc_hp(b[0], 31, 0, level),
                calc_stat(b[1], 31, 0, level, 1.0),
                calc_stat(b[2], 31, 0, level, 1.0),
                calc_stat(b[3], 31, 0, level, 1.0),
                calc_stat(b[4], 31, 0, level, 1.0),
                calc_stat(b[5], 31, 0, level, 1.0),
            ];
        }
    }

    /// Build an opponent's fog-of-war view from an **open team sheet** reveal.
    ///
    /// A sheet is submitted before battle and reveals species/ability/item/moves/Tera
    /// type regardless of anything that happens afterward — those fields are copied
    /// straight from the real `mon` and set to `Known`. Nature/EVs/IVs/stats are never
    /// on a sheet, so they stay at the same sound worst/best-case bounds
    /// `from_opponent_species` already computes — unless `reveal_nature` is set (Open
    /// Team Sheet + Natures), in which case the nature is also copied and the stat
    /// bounds are tightened using that nature's real per-stat modifier instead of the
    /// independent 0.9/1.1 worst-case mix both bounds otherwise assume.
    ///
    /// S34: `force_max_ivs` — `from_opponent_species` always seeds the FULL [0, 31] IV
    /// lattice; it has no way to know about the format's IV-pinning rule. When the
    /// format pins opponent IVs to 31 (Pokémon Champions competitive default,
    /// `InferenceConfig::force_max_ivs`), the min-side stat/BSV bounds this function
    /// hands back must be recomputed at IV 31, not IV 0 — otherwise the stored window
    /// spans a "phantom" region (IV-0-achievable but never IV-31-achievable) that
    /// `pass3_direction_b`'s damage back-solve can narrow into, while `pass5_back_solve`
    /// (which correctly restricts its own search to IV 31 per this same config flag)
    /// can never satisfy — producing "every candidate nature is infeasible" contradictions
    /// on ordinary turns. See the `test_s34_*` regression tests below.
    pub fn from_opponent_open_sheet(
        mon: &PokemonState,
        dex: &HashMap<Species, PokemonData>,
        level: u8,
        reveal_nature: bool,
        force_max_ivs: bool,
    ) -> Self {
        let mut unk = Self::from_opponent_species(mon.species.clone(), dex, level);
        let min_iv: u8 = if force_max_ivs { 31 } else { 0 };
        if force_max_ivs {
            unk.pin_min_ivs_to_max(&mon.species, dex, level);
        }

        unk.possible_abilities = Unknown::Known(mon.ability.clone());
        unk.possible_original_abilities = Unknown::Known(
            mon.original_ability.clone().unwrap_or_else(|| mon.ability.clone()),
        );
        unk.item = Unknown::Known(mon.item.clone());
        unk.known_moves = mon.moves.clone();
        // A sheet reveals move identity, not remaining PP mid-battle — treat this as
        // the team-preview baseline, same as `from_known_pokemon` would before any
        // move has actually been used: current PP = max PP.
        let max_pp = [
            mon.max_pp[0] as i8,
            mon.max_pp[1] as i8,
            mon.max_pp[2] as i8,
            mon.max_pp[3] as i8,
        ];
        unk.move_pp = max_pp;
        unk.max_pp = max_pp;
        unk.possible_tera_type = Unknown::Known(mon.tera_type.clone());

        if reveal_nature {
            unk.possible_natures = Unknown::Known(mon.nature);
            let base = dex
                .get(&mon.species)
                .map(|d| d.base_stats)
                .unwrap_or([100u16; 6]);
            // Nature is now fixed, so both bounds use its REAL per-stat modifier
            // (not the independent 0.9/1.1 worst-case `from_opponent_species` used
            // when the nature was still unknown) — only EV/IV remain uncertain.
            let mods = crate::state::pokemon::nature_stat_modifiers(&mon.nature);
            unk.min_stats = [
                calc_hp(base[0], min_iv, 0, level),
                calc_stat(base[1], min_iv, 0, level, mods[0]),
                calc_stat(base[2], min_iv, 0, level, mods[1]),
                calc_stat(base[3], min_iv, 0, level, mods[2]),
                calc_stat(base[4], min_iv, 0, level, mods[3]),
                calc_stat(base[5], min_iv, 0, level, mods[4]),
            ];
            unk.max_stats = [
                calc_hp(base[0], 31, 252, level),
                calc_stat(base[1], 31, 252, level, mods[0]),
                calc_stat(base[2], 31, 252, level, mods[1]),
                calc_stat(base[3], 31, 252, level, mods[2]),
                calc_stat(base[4], 31, 252, level, mods[3]),
                calc_stat(base[5], 31, 252, level, mods[4]),
            ];
            // min/max_pre_nature_stat unchanged — nature doesn't affect BSV.
        }

        unk
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

    /// Like [`team_preview_from_perspective`](Self::team_preview_from_perspective), but
    /// for a non-perfect [`InformationMode`] where the opponent's team is revealed via
    /// an open team sheet rather than starting fully unknown. Takes the opponent's
    /// **actual** parsed team (not just species) since open-sheet reveals need ground
    /// truth for moves/item/ability/Tera type. A separate function (rather than adding
    /// a `mode` parameter to `team_preview_from_perspective`) keeps the existing
    /// species-only path — and its tests — untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn team_preview_open_sheet_from_perspective(
        viewer: Player,
        my_team: &[PokemonState],
        opponent_team: &[PokemonState],
        dex: &HashMap<Species, PokemonData>,
        active_per_side: u8,
        brought_per_side: u8,
        level: u8,
        mode: InformationMode,
        force_max_ivs: bool,
    ) -> UnknownMatchState {
        let my_mons: Vec<UnknownPokemonState> = my_team
            .iter()
            .map(UnknownPokemonState::from_known_pokemon)
            .collect();
        let reveal_nature = mode == InformationMode::OpenTeamSheetNatures;
        let opp_mons: Vec<UnknownPokemonState> = opponent_team
            .iter()
            .map(|mon| {
                UnknownPokemonState::from_opponent_open_sheet(mon, dex, level, reveal_nature, force_max_ivs)
            })
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

    /// Build a **closed team sheet** team-preview fog-of-war state: the opponent's
    /// species are visible (as shown on the team-preview screen) but nothing else —
    /// moves/item/ability/nature/EVs/IVs/Tera type all stay at the same sound
    /// worst/best-case bounds `from_opponent_species` computes for a fully-unknown
    /// mon. This is the traditional VGC/Champions competitive format, as opposed to
    /// `team_preview_open_sheet_from_perspective`'s early reveal.
    ///
    /// Takes the opponent's **actual** parsed team (not just species), mirroring
    /// `team_preview_open_sheet_from_perspective`'s signature, so callers with a
    /// ground-truth `TeamPreviewState` (e.g. `routes.rs`) can pass `&preview.p2_mons`
    /// directly without re-deriving a species list — only `mon.species` is read.
    ///
    /// When `force_max_ivs` is set (Pokémon Champions competitive default), the
    /// opponent's min-side IV/stat bounds are tightened to IV 31 via
    /// `pin_min_ivs_to_max` — see that function's doc comment (S34) for why the
    /// untightened `[0, 31]` bounds `from_opponent_species` assumes would otherwise
    /// produce inference contradictions under a format that guarantees IV 31.
    #[allow(clippy::too_many_arguments)]
    pub fn team_preview_closed_sheet_from_perspective(
        viewer: Player,
        my_team: &[PokemonState],
        opponent_team: &[PokemonState],
        dex: &HashMap<Species, PokemonData>,
        active_per_side: u8,
        brought_per_side: u8,
        level: u8,
        force_max_ivs: bool,
    ) -> UnknownMatchState {
        let my_mons: Vec<UnknownPokemonState> = my_team
            .iter()
            .map(UnknownPokemonState::from_known_pokemon)
            .collect();
        let opp_mons: Vec<UnknownPokemonState> = opponent_team
            .iter()
            .map(|mon| {
                let mut unk = UnknownPokemonState::from_opponent_species(mon.species.clone(), dex, level);
                if force_max_ivs {
                    unk.pin_min_ivs_to_max(&mon.species, dex, level);
                }
                unk
            })
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
