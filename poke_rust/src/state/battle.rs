use crate::data::item::Item;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::state::dex_data::{
    PokemonData, PseudoWeather, SelfSwitchType, SideCondition, SlotCondition, Terrain, Weather,
};
use crate::state::pokemon::PokemonState;
use crate::information::information::InformationEvent;
use std::collections::HashMap;

fn humanize_identifier(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    let mut result = String::new();
    let mut previous: Option<char> = None;

    for current in value.chars() {
        let insert_space = match previous {
            Some(prev) => {
                (prev.is_ascii_lowercase() && current.is_ascii_uppercase())
                    || (prev.is_ascii_digit() && current.is_ascii_alphabetic())
                    || (prev.is_ascii_alphabetic() && current.is_ascii_digit())
            }
            None => false,
        };

        if insert_space && !result.ends_with(' ') {
            result.push(' ');
        }

        result.push(current);
        previous = Some(current);
    }

    result
}

fn species_name(species: &Species) -> String {
    humanize_identifier(format!("{:?}", species))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Player {
    P1,
    P2,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldSlot {
    pub player: Player,
    pub slot_index: u8,
}

impl std::fmt::Debug for FieldSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = match self.player {
            Player::P1 => "P1",
            Player::P2 => "P2",
        };
        write!(f, "{}_{}", p, self.slot_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MoveAction {
    pub move_name: PokemonMove,
    pub priority: i8,
    pub user_slot: FieldSlot,
    pub target_slot: Option<FieldSlot>,
    /// True when Quick Claw (20%) or Quick Draw (30%) activated this turn.
    /// Probability is combined as 1 − (1−p_qc)(1−p_qd) per holder; decided at turn start.
    /// A mon with only Quick Claw retains the same 20% chance as before.
    pub moves_first: bool,
    /// Set by Quash: holder acts last within its priority bracket, after all non-Quashed
    /// Pokémon. moves_first overrides moves_last (After You beats Quash).
    pub moves_last: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SwitchAction {
    pub user_slot: FieldSlot,
    pub switch_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MegaAction {
    pub user_slot: FieldSlot,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TeraAction {
    pub user_slot: FieldSlot,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    MoveAction(MoveAction),
    SwitchAction(SwitchAction),
    MegaAction(MegaAction),
    TeraAction(TeraAction),
}

/// `last_move_on_field` is intentionally excluded from `PartialEq`, `Eq`, and `Hash`.
/// That field tracks ephemeral "which move ran last" info for Copycat; including it in
/// equality would prevent `coalesce_branches` from merging states that differ only in
/// turn-order (e.g. two speed-tie orderings that otherwise produce identical game state).
#[derive(Debug, Clone)]
pub struct BattleState {
    pub active_per_side: u8,

    pub p1_active_mons: Vec<PokemonState>,
    pub p2_active_mons: Vec<PokemonState>,
    pub p1_back_mons: Vec<PokemonState>,
    pub p2_back_mons: Vec<PokemonState>,

    pub action_queue: Vec<Action>,

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
    pub weather_turns: Option<u8>,
    pub pseudo_weathers: Vec<PseudoWeather>,
    pub pseudo_weather_turns: Vec<u8>,
    pub terrain: Option<Terrain>,
    pub terrain_turns: Option<u8>,
    pub p1_side_conditions: Vec<SideCondition>,
    pub p1_side_condition_turns: Vec<u8>,
    pub p2_side_conditions: Vec<SideCondition>,
    pub p2_side_condition_turns: Vec<u8>,
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

    /// Set to true by `note_move_outcome` when a Pokémon is prevented from acting (Cant).
    /// Causes the `MoveUsed` wrapper in `execute_action` to be suppressed for that branch
    /// (Showdown emits `|cant|` instead of `|move|` in this case). Reset at the start of
    /// every `possible_damage_outcomes_for_move`. Excluded from PartialEq/Hash.
    pub move_was_prevented: bool,

    /// The target slots the engine actually resolved for the current move (auto-targeting,
    /// redirection, Sucker-Punch-style overrides). The command's `target_slot` is `None`
    /// for auto-targeted moves, so the `MoveUsed` wrapper reads this to populate
    /// `MoveUsed::targets` — without it the event stream reports empty targets and the
    /// inference passes keyed on targets never fire. Consumed (taken) by the wrapper.
    /// Excluded from PartialEq/Hash.
    pub resolved_move_targets: Vec<FieldSlot>,

    /// Flat push-stream of events observed this turn. Wrapped into causal `reactions`
    /// trees by `execute_action` and `step_action_queue` using the split_off trick.
    /// Excluded from `PartialEq`/`Hash` so internal coalescing is unchanged.
    pub pending_events: Vec<InformationEvent>,

    /// When `Some(p)`, events are recorded from player `p`'s perspective.
    /// `None` = no collection (zero-overhead path).
    /// Excluded from `PartialEq`/`Hash`.
    pub event_observer: Option<Player>,
}

/// Format a single Pokémon's state as a multi-line string for display.
fn format_mon(m: &PokemonState) -> String {
    let stat_names = ["HP", "Atk", "Def", "SpA", "SpD", "Spe"];
    let stats_str = stat_names
        .iter()
        .enumerate()
        .map(|(i, name)| format!("{}: {}", name, m.stats[i]))
        .collect::<Vec<_>>()
        .join(", ");

    let boost_names = ["Atk", "Def", "SpA", "SpD", "Spe", "Acc", "Eva"];
    let active_boosts: Vec<String> = m
        .boosts
        .iter()
        .enumerate()
        .filter(|(_, b)| **b != 0)
        .map(|(i, b)| format!("{}{:+}", boost_names[i], b))
        .collect();
    let boosts_str = if active_boosts.is_empty() {
        "none".to_string()
    } else {
        active_boosts.join(", ")
    };

    let status_str = m
        .status
        .as_ref()
        .map(|s| format!("{:?}", s))
        .unwrap_or_else(|| "Healthy".to_string());
    let vol_str = if m.volatiles.is_empty() {
        "none".to_string()
    } else {
        m.volatiles
            .iter()
            .map(|v| format!("{:?}", v))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let tera_info = if m.is_tera {
        format!("Tera({:?})", m.tera_type)
    } else {
        "No Tera".to_string()
    };
    let mega_info = if m.has_mega_form {
        m.mega_species
            .as_ref()
            .map(|s| format!("Mega({:?})", s))
            .unwrap_or_else(|| "Has Mega (unknown species)".to_string())
    } else {
        "No Mega".to_string()
    };
    let moves_str = m
        .moves
        .iter()
        .enumerate()
        .map(|(i, mov)| {
            let name = mov
                .as_ref()
                .map(|mv| humanize_identifier(format!("{:?}", mv)))
                .unwrap_or_else(|| format!("Move {}", i + 1));
            format!("{} (PP {})", name, m.move_pp.get(i).copied().unwrap_or(0))
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "{} ({}/{} HP), Status: {}{}\n    Stats: {}\n    Boosts: {}\n    Volatiles: {}\n    {} | {}\n    Moves: {}",
        species_name(&m.species),
        m.hp,
        m.stats[0],
        status_str,
        if m.item != crate::data::item::Item::None {
            format!(", Item: {:?}, Ability: {:?}", m.item, m.ability)
        } else {
            format!(", Ability: {:?}", m.ability)
        },
        stats_str,
        boosts_str,
        vol_str,
        tera_info,
        mega_info,
        moves_str,
    )
}

/// Write a labelled team section (active or back) to `f`.
fn write_team_section(
    f: &mut std::fmt::Formatter<'_>,
    label: &str,
    mons: &[PokemonState],
) -> std::fmt::Result {
    writeln!(f, "{}:", label)?;
    if mons.is_empty() {
        writeln!(f, "  (none)")
    } else {
        writeln!(
            f,
            "  {}",
            mons.iter().map(format_mon).collect::<Vec<_>>().join("\n  ")
        )
    }
}

impl std::fmt::Display for BattleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Turn {} (Started: {}, Ended: {})",
            self.turn_number, self.turn_started, self.turn_ended
        )?;

        write_team_section(f, "P1 Active", &self.p1_active_mons)?;
        write_team_section(f, "P1 Back", &self.p1_back_mons)?;
        writeln!(
            f,
            "P1 Has Tera: {} | Has Mega: {}",
            self.p1_has_tera, self.p1_has_mega
        )?;
        write_team_section(f, "P2 Active", &self.p2_active_mons)?;
        write_team_section(f, "P2 Back", &self.p2_back_mons)?;
        writeln!(
            f,
            "P2 Has Tera: {} | Has Mega: {}",
            self.p2_has_tera, self.p2_has_mega
        )?;

        if let Some(weather) = &self.weather {
            if let Some(turns) = self.weather_turns {
                writeln!(f, "Weather: {:?} ({}t)", weather, turns)?;
            } else {
                writeln!(f, "Weather: {:?}", weather)?;
            }
        }

        if !self.pseudo_weathers.is_empty() {
            let pseudo_strs: Vec<String> = self
                .pseudo_weathers
                .iter()
                .zip(self.pseudo_weather_turns.iter())
                .map(|(pw, turns)| format!("{:?} ({}t)", pw, turns))
                .collect();
            writeln!(f, "Pseudo-Weather: {}", pseudo_strs.join(", "))?;
        }

        if let Some(terrain) = &self.terrain {
            if let Some(turns) = self.terrain_turns {
                writeln!(f, "Terrain: {:?} ({}t)", terrain, turns)?;
            } else {
                writeln!(f, "Terrain: {:?}", terrain)?;
            }
        }

        if !self.p1_side_conditions.is_empty() {
            let p1_side_strs: Vec<String> = self
                .p1_side_conditions
                .iter()
                .zip(self.p1_side_condition_turns.iter())
                .map(|(sc, turns)| format!("{:?} ({}t)", sc, turns))
                .collect();
            writeln!(f, "P1 Side Conditions: {}", p1_side_strs.join(", "))?;
        }

        if !self.p2_side_conditions.is_empty() {
            let p2_side_strs: Vec<String> = self
                .p2_side_conditions
                .iter()
                .zip(self.p2_side_condition_turns.iter())
                .map(|(sc, turns)| format!("{:?} ({}t)", sc, turns))
                .collect();
            writeln!(f, "P2 Side Conditions: {}", p2_side_strs.join(", "))?;
        }

        let p1_has_slot_conds = self
            .p1_slot_conditions
            .iter()
            .any(|slot_conds| !slot_conds.is_empty());
        if p1_has_slot_conds {
            for (slot_idx, slot_conds) in self.p1_slot_conditions.iter().enumerate() {
                if !slot_conds.is_empty() {
                    let slot_strs: Vec<String> =
                        slot_conds.iter().map(|sc| format!("{:?}", sc)).collect();
                    writeln!(f, "  P1 Slot {}: {}", slot_idx, slot_strs.join(", "))?;
                }
            }
        }

        let p2_has_slot_conds = self
            .p2_slot_conditions
            .iter()
            .any(|slot_conds| !slot_conds.is_empty());
        if p2_has_slot_conds {
            for (slot_idx, slot_conds) in self.p2_slot_conditions.iter().enumerate() {
                if !slot_conds.is_empty() {
                    let slot_strs: Vec<String> =
                        slot_conds.iter().map(|sc| format!("{:?}", sc)).collect();
                    writeln!(f, "  P2 Slot {}: {}", slot_idx, slot_strs.join(", "))?;
                }
            }
        }

        if !self.action_queue.is_empty() {
            writeln!(f, "Action Queue:")?;
            for (i, action) in self.action_queue.iter().enumerate() {
                writeln!(f, "  {}: {:?}", i, action)?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TeamPreviewState {
    pub active_per_side: u8,
    pub brought_per_side: u8,
    pub p1_mons: Vec<PokemonState>,
    pub p2_mons: Vec<PokemonState>,
}

#[derive(Debug, Clone)]
pub enum MatchState {
    BattleState(BattleState),
    TeamPreviewState(TeamPreviewState),
    GameOverState {
        winner: Player,
        /// The finished battle's undrained observer event log (the final turn's
        /// attack/faint events), carried over so the winning turn still has a log.
        /// Empty when no observer is attached — matching `BattleState`'s
        /// Eq/Hash convention, which only distinguishes event histories when
        /// an observer is active.
        pending_events: Vec<InformationEvent>,
        /// The field as it stood when the battle ended (fainted mon, final HP),
        /// for display purposes — the UI renders it behind the winner overlay.
        /// Deliberately EXCLUDED from Eq/Hash below: game-over branches coalesce
        /// by winner + event history exactly as before this field existed; a
        /// merged branch keeps one representative final state.
        final_state: Box<BattleState>,
    },
}

impl PartialEq for MatchState {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (MatchState::BattleState(a), MatchState::BattleState(b)) => a == b,
            (MatchState::TeamPreviewState(a), MatchState::TeamPreviewState(b)) => a == b,
            (
                MatchState::GameOverState { winner: w1, pending_events: e1, final_state: _ },
                MatchState::GameOverState { winner: w2, pending_events: e2, final_state: _ },
            ) => w1 == w2 && e1 == e2,
            _ => false,
        }
    }
}

impl Eq for MatchState {}

impl std::hash::Hash for MatchState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            MatchState::BattleState(bs) => bs.hash(state),
            MatchState::TeamPreviewState(tp) => tp.hash(state),
            MatchState::GameOverState { winner, pending_events, final_state: _ } => {
                winner.hash(state);
                pending_events.hash(state);
            }
        }
    }
}

impl std::fmt::Display for MatchState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchState::BattleState(bs) => write!(f, "{}", bs),
            MatchState::TeamPreviewState(tp) => {
                write!(
                    f,
                    "TeamPreview: P1={} mons, P2={} mons",
                    tp.p1_mons.len(),
                    tp.p2_mons.len()
                )
            }
            MatchState::GameOverState { winner, .. } => {
                let w = match winner {
                    Player::P1 => "P1",
                    Player::P2 => "P2",
                };
                write!(f, "GameOver: winner={}", w)
            }
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct AttackCommand {
    pub move_slot: usize,
    pub target: Option<FieldSlot>,
    pub terastallize: bool,
    pub mega_evolve: bool,
}

impl std::fmt::Debug for AttackCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Atk({}", self.move_slot)?;
        if let Some(t) = &self.target {
            write!(f, "->{:?}", t)?;
        }
        if self.terastallize {
            write!(f, " TERA")?;
        }
        if self.mega_evolve {
            write!(f, " MEGA")?;
        }
        write!(f, ")")
    }
}

#[derive(Clone, PartialEq)]
pub struct SwitchCommand {
    pub party_index: usize,
}

impl std::fmt::Debug for SwitchCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sw({})", self.party_index)
    }
}

#[derive(Clone, PartialEq)]
pub enum BattleCommand {
    Attack(AttackCommand),
    Switch(SwitchCommand),
    /// Forced when the holder has no usable move (all PP exhausted, or choice-locked move is out of PP).
    /// Carries no tera/mega fields — the holder cannot Tera or Mega Evolve on a Struggle turn.
    Struggle {
        target: Option<FieldSlot>,
    },
    Pass,
}

impl std::fmt::Debug for BattleCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BattleCommand::Attack(a) => write!(f, "{:?}", a),
            BattleCommand::Switch(s) => write!(f, "{:?}", s),
            BattleCommand::Struggle { target } => {
                write!(f, "Struggle")?;
                if let Some(t) = target {
                    write!(f, "->{:?}", t)?;
                }
                Ok(())
            }
            BattleCommand::Pass => write!(f, "Pass"),
        }
    }
}

#[derive(Clone)]
pub struct TeamPreviewCommand {
    pub active_indices: Vec<usize>,
    pub back_indices: Vec<usize>,
}

impl std::fmt::Debug for TeamPreviewCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Preview({:?} | {:?})",
            self.active_indices, self.back_indices
        )
    }
}

#[derive(Clone)]
pub enum PlayerCommand {
    Battle(Vec<BattleCommand>),
    Pass,
    TeamPreview(TeamPreviewCommand),
}

impl std::fmt::Debug for PlayerCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayerCommand::Battle(cmds) => {
                write!(f, "Battle[")?;
                for (i, cmd) in cmds.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:?}", cmd)?;
                }
                write!(f, "]")
            }
            PlayerCommand::TeamPreview(cmd) => write!(f, "{:?}", cmd),
            PlayerCommand::Pass => write!(f, "Pass"),
        }
    }
}

/// Change a Pokémon's in-battle form to `new_species`, recomputing types, stats, and
/// weight from the dex entry. Current HP is preserved as a fraction of max HP (a no-op
/// for forms sharing an HP base, e.g. Aegislash and Palafin). The ability is NOT
/// touched — form-change abilities (Stance Change, Zero to Hero, …) keep their ability;
/// Mega Evolution overrides it separately. Returns false if the dex has no entry.
pub fn change_form(
    mon: &mut PokemonState,
    new_species: Species,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> bool {
    let Some(form_data) = pokemon_dex.get(&new_species) else {
        return false;
    };

    let old_max_hp = mon.stats[0].max(1);
    let hp_ratio = mon.hp.min(old_max_hp) as f32 / old_max_hp as f32;
    let stats = crate::state::pokemon::calc_stats_for_level(
        form_data.base_stats,
        mon.ivs,
        mon.evs,
        mon.level,
        &mon.nature,
    );
    let new_max_hp = stats[0].max(1);
    let scaled_hp = (hp_ratio * new_max_hp as f32).floor() as u16;
    let hp = if mon.hp == 0 {
        0
    } else {
        scaled_hp.clamp(1, new_max_hp)
    };

    mon.species = new_species;
    mon.types = form_data.types.clone();
    mon.stats = stats;
    mon.hp = hp;
    mon.weight_hg = form_data.weight;
    true
}

/// Applies Mega Evolution to a Pokemon if it is eligible.
/// Returns true if Mega Evolution was applied.
pub fn try_mega_evolution(
    mon: &mut PokemonState,
    pokemon_dex: &HashMap<Species, PokemonData>,
) -> bool {
    if mon.fainted || mon.is_mega || !mon.has_mega_form {
        return false;
    }

    let mega_species_key = match mon.mega_species.clone() {
        Some(key) => key,
        None => return false,
    };

    let mega_ability = pokemon_dex
        .get(&mega_species_key)
        .and_then(|d| d.primary_ability.clone());

    if !change_form(mon, mega_species_key, pokemon_dex) {
        return false;
    }

    if let Some(ability) = mega_ability {
        mon.ability = ability.clone();
    }
    mon.is_mega = true;
    mon.has_mega_form = false;
    mon.mega_species = None;

    true
}

// ── Custom PartialEq / Eq / Hash for BattleState ─────────────────────────────
// `last_move_on_field` is excluded from equality and hashing so that
// `coalesce_branches` can merge states that differ only in move-order tracking.
//
// S2 soundness fix: `pending_events` is compared/hashed ONLY when `event_observer`
// is `Some` (observability active). Two branches can reach identical battle state
// via DIFFERENT observable histories (e.g. a Crit vs a non-Crit hit that happen to
// deal the same damage) — `coalesce_branches` (called throughout turn expansion via
// `MatchState`'s derived `PartialEq`/`Hash`, which delegates to this impl for the
// `BattleState` variant) must not silently merge them and drop one branch's event
// history while summing both probabilities onto the survivor, or the inference
// engine would treat a coin-flip fact (e.g. `is_crit`) as certain. When no observer
// is attached, `pending_events` is always empty anyway, so gating the comparison on
// `event_observer` leaves the hot (non-observed) simulation path byte-for-byte
// unaffected. `event_observer` itself is only read here, not compared, but every
// branch within one `simulate_turn` call carries the same observer value throughout
// (set once on the initial branches and propagated by cloning), so this is sound in
// practice; only `self.event_observer` is consulted since both sides of any real
// comparison always agree.
impl PartialEq for BattleState {
    fn eq(&self, other: &Self) -> bool {
        self.active_per_side == other.active_per_side
            && self.p1_active_mons == other.p1_active_mons
            && self.p2_active_mons == other.p2_active_mons
            && self.p1_back_mons == other.p1_back_mons
            && self.p2_back_mons == other.p2_back_mons
            && self.action_queue == other.action_queue
            && self.turn_number == other.turn_number
            && self.turn_started == other.turn_started
            && self.turn_ended == other.turn_ended
            && self.p1_has_tera == other.p1_has_tera
            && self.p2_has_tera == other.p2_has_tera
            && self.p1_has_mega == other.p1_has_mega
            && self.p2_has_mega == other.p2_has_mega
            && self.weather == other.weather
            && self.weather_turns == other.weather_turns
            && self.pseudo_weathers == other.pseudo_weathers
            && self.pseudo_weather_turns == other.pseudo_weather_turns
            && self.terrain == other.terrain
            && self.terrain_turns == other.terrain_turns
            && self.p1_side_conditions == other.p1_side_conditions
            && self.p1_side_condition_turns == other.p1_side_condition_turns
            && self.p2_side_conditions == other.p2_side_conditions
            && self.p2_side_condition_turns == other.p2_side_condition_turns
            && self.p1_slot_conditions == other.p1_slot_conditions
            && self.p2_slot_conditions == other.p2_slot_conditions
            && self.self_switch_pending == other.self_switch_pending
            && self.items_consumed_this_turn == other.items_consumed_this_turn
            && (self.event_observer.is_none() || self.pending_events == other.pending_events)
        // last_move_on_field, sub_damage_dealt, round_used_this_turn,
        // move_was_prevented, event_observer intentionally excluded
    }
}

impl Eq for BattleState {}

impl std::hash::Hash for BattleState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.active_per_side.hash(state);
        self.p1_active_mons.hash(state);
        self.p2_active_mons.hash(state);
        self.p1_back_mons.hash(state);
        self.p2_back_mons.hash(state);
        self.action_queue.hash(state);
        self.turn_number.hash(state);
        self.turn_started.hash(state);
        self.turn_ended.hash(state);
        self.p1_has_tera.hash(state);
        self.p2_has_tera.hash(state);
        self.p1_has_mega.hash(state);
        self.p2_has_mega.hash(state);
        self.weather.hash(state);
        self.weather_turns.hash(state);
        self.pseudo_weathers.hash(state);
        self.pseudo_weather_turns.hash(state);
        self.terrain.hash(state);
        self.terrain_turns.hash(state);
        self.p1_side_conditions.hash(state);
        self.p1_side_condition_turns.hash(state);
        self.p2_side_conditions.hash(state);
        self.p2_side_condition_turns.hash(state);
        self.p1_slot_conditions.hash(state);
        self.p2_slot_conditions.hash(state);
        self.self_switch_pending.hash(state);
        self.items_consumed_this_turn.hash(state);
        // Consistent with PartialEq above: only hashed when observability is active.
        if self.event_observer.is_some() {
            self.pending_events.hash(state);
        }
        // last_move_on_field, sub_damage_dealt, round_used_this_turn,
        // move_was_prevented, event_observer intentionally excluded
    }
}
