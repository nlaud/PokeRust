use crate::data::item::Item;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Unknown<T> {
    Known(T),
    Not(Vec<T>),
    Possibly(Vec<T>),
}
pub enum PokemonHP {
    Number(u16), //Allies use number
    Percent(u8), //Opponents use percent
}

pub struct UnknownPokemonState {
    pub possible_mon_id: Unkown<u8>, //1:1 with species if there is only 1 of each pokemon
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

    // ── Per-turn event flags (cleared in end_turn Phase 5 and on switch-out) ────────
    /// Took any damage this turn — direct hits, recoil, confusion self-hits, etc.
    /// Read by Assurance's ×2 condition.
    pub damaged_this_turn: bool,
    /// Slots whose direct move hits damaged this Pokémon this turn. Read by Avalanche
    /// ("the target damaged the user this turn").
    pub damaged_by_this_turn: Vec<crate::battle::FieldSlot>,
    /// Damage taken from the most recent physical direct-move hit this turn, and the slot
    /// that landed it. Overwritten per-hit (multi-hit: final hit wins). Read by Counter.
    pub last_physical_damage_taken: PokemonHP,
    pub last_physical_attacker: Option<crate::battle::FieldSlot>,
    /// Damage taken from the most recent special direct-move hit this turn, and the slot
    /// that landed it. Read by Mirror Coat.
    pub last_special_damage_taken: PokemonHP,
    pub last_special_attacker: Option<crate::battle::FieldSlot>,
    /// Damage taken from the most recent direct-move hit this turn (any category), and the
    /// slot that landed it. Read by Metal Burst and Comeuppance.
    pub last_damage_taken: PokemonHP,
    pub last_damage_attacker: Option<crate::battle::FieldSlot>,
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
    pub pre_transform: Option<Box<PokemonState>>, //Don't need to make this unknown unless pokemon with imposter + transform exists

    /// Original types saved when Mimicry overwrites them. Restored when terrain ends or
    /// the holder switches out. `None` when Mimicry is not active.
    pub pre_mimicry_types: Option<Vec<crate::dex_data::PokemonType>>,

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
        stat: Stat,
    },
    NatureNerfsStat {
        mon_idx: usize,
        stat: Stat,
    },
    EVIVStatGE {
        mon_idx: usize,
        stat: Stat,
        value: u16,
    }, //Stats FROM EVs and IVs greater than or equal to a value
}
