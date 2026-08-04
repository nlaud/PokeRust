//! Scores a nonterminal position at the search limit.
//!
//! The score is P1's win probability in the range `[0, 1]`.
//! This range supplies valid bounds for serialized alpha-beta and star1 pruning.
//!
//! [`SolveConfig`](super::SolveConfig) stores the evaluator as a function pointer.
//! Callers can replace it without a search change.
//!
//! # The feature frame
//!
//! [`features`] returns one value for each named feature.
//! Each value is a P1 quantity minus the matching P2 quantity.
//! The score is `logistic(weights . features)`.
//! The model holds no bias term.
//!
//! A mirrored position negates every feature.
//! The logistic map then returns one minus the original score.
//! Side-swap symmetry therefore holds for every weight vector.
//!
//! # Two weight vectors
//!
//! [`heuristic`] uses the hand-set weights in [`HAND_WEIGHTS`].
//! [`fitted`] uses the weights that `bin/train_eval` produced.
//! `weights/eval_v1.json` holds the fitted vector.
//! Record a weight change in `benches/RESULTS.md`.
//!
//! # The context
//!
//! [`EvalContext`] carries the dexes and an optional belief.
//! The threat features read the move dex, so an evaluator needs more than the
//! position.
//! Every current caller passes `None` for the belief.
//!
//! # Batches
//!
//! [`score_batch`] scores a slice of positions.
//! It calls [`BatchEvaluator`] when the caller supplies one.
//! It loops the scalar evaluator otherwise.
//! Both searches are depth first, so neither reaches more than one leaf at a
//! time.
//! A model evaluator and a parallel search need this entry point.
//!
//! # The policy
//!
//! [`policy_scores`] returns a softmax over one player's joint actions.
//! [`MctsConfig::policy_prior`](super::mcts::MctsConfig::policy_prior) orders an
//! action list by that score.
//! No other consumer reads the policy.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::unknowns::UnknownBattleState;
use crate::simulator::{DamageConfig, get_possible_commands_for_active_slot};
use crate::simulator::helpers::{
    accuracy_hit_probability, calculate_damage_outcomes_for_target, effective_move_priority,
    effective_speed_for_slot,
};
use crate::state::battle::{
    BattleCommand, BattleState, FieldSlot, Player, try_mega_evolution,
};
use crate::state::dex_data::{
    MoveCategory, MoveData, PokemonData, PseudoWeather, SideCondition, Status,
};
use crate::state::pokemon::PokemonState;

/// Everything an evaluator needs beyond the position itself.
///
/// The threat features read the move dex.
/// A later model evaluator can read the species dex and the belief.
#[derive(Debug, Clone, Copy)]
pub struct EvalContext<'a> {
    pub pokemon_dex: &'a HashMap<Species, PokemonData>,
    pub move_dex: &'a HashMap<PokemonMove, MoveData>,
    /// The belief that the position was drawn from.
    /// Every current caller passes `None`, because the solver takes a concrete
    /// state.
    pub belief: Option<&'a UnknownBattleState>,
}

impl<'a> EvalContext<'a> {
    /// Builds a context without a belief.
    pub fn new(
        pokemon_dex: &'a HashMap<Species, PokemonData>,
        move_dex: &'a HashMap<PokemonMove, MoveData>,
    ) -> Self {
        EvalContext {
            pokemon_dex,
            move_dex,
            belief: None,
        }
    }
}

/// Scores a nonterminal state as P1's win probability.
/// Results must stay from zero through one.
/// A mirrored state must return one minus the original score.
pub type LeafEvaluator = fn(&BattleState, &EvalContext<'_>) -> f64;

/// Scores a slice of positions in one call.
///
/// The implementation must write one score for each position, in order, into
/// the output buffer.
/// It must clear the buffer first.
pub type BatchEvaluator = fn(&[&BattleState], &EvalContext<'_>, &mut Vec<f64>);

/// Scores each position of `states` into `out`.
///
/// `batch` runs when the caller supplies it.
/// The scalar evaluator runs once per position otherwise.
pub fn score_batch(
    states: &[&BattleState],
    ctx: &EvalContext<'_>,
    eval: LeafEvaluator,
    batch: Option<BatchEvaluator>,
    out: &mut Vec<f64>,
) {
    match batch {
        Some(batch) => batch(states, ctx, out),
        None => {
            out.clear();
            out.extend(states.iter().map(|state| eval(state, ctx)));
        }
    }
}

// ── The feature frame ───────────────────────────────────────────────────────

/// How many features the value model holds.
pub const FEATURE_COUNT: usize = 13;

/// Names the features in index order.
/// `weights/eval_v1.json` stores a value against each name.
pub const FEATURE_NAMES: [&str; FEATURE_COUNT] = [
    "health",
    "status",
    "boosts",
    "accuracy_evasion",
    "hazards",
    "threat",
    "guaranteed_kill",
    "possible_kill",
    "speed",
    "protect",
    "tera",
    "mega",
    "screens",
];

/// One value for each feature, in the order of [`FEATURE_NAMES`].
pub type Features = [f64; FEATURE_COUNT];

/// The hand-set weights of [`heuristic`].
///
/// The first five entries reproduce the health, status, boost, and hazard terms
/// of the original evaluator.
/// The other eight are initial values for the matchup features.
pub const HAND_WEIGHTS: Features = [
    1.0,  // health
    -1.0, // status
    0.06, // boosts
    0.03, // accuracy_evasion
    -1.0, // hazards
    0.30, // threat
    0.35, // guaranteed_kill
    0.20, // possible_kill
    0.10, // speed
    0.05, // protect
    0.15, // tera
    0.15, // mega
    0.10, // screens
];

/// Fraction of a Pokémon score that does not depend on remaining HP.
const ALIVE_SHARE: f64 = 0.5;

/// One layer of an entry hazard on a side, in Pokemon-equivalents. Charged
/// against the side that has to switch into it.
const HAZARD_WEIGHT: f64 = 0.12;

/// Logistic scale in units of one Pokémon.
/// [`train`](super::train) reads it, because the gradient of the loss carries
/// this factor.
pub const LOGISTIC_SCALE: f64 = 0.8;

/// The damage settings of the threat features.
///
/// One roll keeps a leaf cheap. The critical-hit branch stays out, because the
/// evaluator compares a matchup instead of two moves of one slot.
const EVAL_DAMAGE_CONFIG: DamageConfig = DamageConfig {
    consider_crit: false,
    damage_rolls: 1,
    sample: false,
};

/// Calculates the default position score.
/// An even position returns 0.5.
pub fn heuristic(state: &BattleState, ctx: &EvalContext<'_>) -> f64 {
    score_with(state, ctx, &HAND_WEIGHTS)
}

/// Scores a position with the trained weights.
///
/// `weights/eval_v1.json` supplies the vector.
/// A name that the file omits keeps its [`HAND_WEIGHTS`] value.
pub fn fitted(state: &BattleState, ctx: &EvalContext<'_>) -> f64 {
    score_with(state, ctx, fitted_weights())
}

/// Returns 0.5 for each nonterminal state.
/// Tests use it to isolate search behavior from the heuristic.
pub fn even(_state: &BattleState, _ctx: &EvalContext<'_>) -> f64 {
    0.5
}

/// Maps one weight vector and one position onto a win probability.
pub fn score_with(state: &BattleState, ctx: &EvalContext<'_>, weights: &Features) -> f64 {
    logistic(dot(weights, &features(state, ctx)))
}

/// Returns the antisymmetric feature vector of `state`.
///
/// Each entry is a P1 quantity minus the matching P2 quantity.
pub fn features(state: &BattleState, ctx: &EvalContext<'_>) -> Features {
    let p1 = side_features(state, Player::P1, ctx);
    let p2 = side_features(state, Player::P2, ctx);
    let mut out = [0.0; FEATURE_COUNT];
    for ((slot, own), theirs) in out.iter_mut().zip(p1.iter()).zip(p2.iter()) {
        *slot = own - theirs;
    }
    out
}

/// The dot product of a weight vector and a feature vector.
fn dot(weights: &Features, values: &Features) -> f64 {
    weights
        .iter()
        .zip(values.iter())
        .map(|(weight, value)| weight * value)
        .sum()
}

/// One side's own quantities, before the subtraction.
fn side_features(state: &BattleState, player: Player, ctx: &EvalContext<'_>) -> Features {
    let (active, bench, side_conditions, has_tera, has_mega) = match player {
        Player::P1 => (
            &state.p1_active_mons,
            &state.p1_back_mons,
            &state.p1_side_conditions,
            state.p1_has_tera,
            state.p1_has_mega,
        ),
        Player::P2 => (
            &state.p2_active_mons,
            &state.p2_back_mons,
            &state.p2_side_conditions,
            state.p2_has_tera,
            state.p2_has_mega,
        ),
    };

    let mut out = [0.0; FEATURE_COUNT];
    for mon in active.iter().chain(bench.iter()) {
        out[0] += alive_score(mon);
        out[1] += status_penalty(mon);
    }
    for mon in active.iter() {
        if mon.fainted {
            continue;
        }
        let offensive_defensive: i32 = mon.boosts[..5].iter().map(|&b| b as i32).sum();
        let accuracy_evasion: i32 = mon.boosts[5..].iter().map(|&b| b as i32).sum();
        out[2] += f64::from(offensive_defensive);
        out[3] += f64::from(accuracy_evasion);
        out[9] += protect_pressure(mon, ctx);
    }
    out[4] = hazard_penalty(side_conditions);
    out[10] = flag(has_tera);
    // A Mega right is worth nothing without a living Pokemon that can spend it.
    // Every side keeps the right, and most teams carry no Mega stone.
    out[11] = flag(
        has_mega
            && active
                .iter()
                .chain(bench.iter())
                .any(|mon| !mon.fainted && mon.hp > 0 && mon.has_mega_form),
    );
    out[12] = screen_count(side_conditions);

    let (threat, guaranteed, possible, speed) = matchup_features(state, player, ctx);
    out[5] = threat;
    out[6] = guaranteed;
    out[7] = possible;
    out[8] = speed;
    out
}

/// Scores one Pokémon from health alone.
fn alive_score(mon: &PokemonState) -> f64 {
    if mon.fainted || mon.hp == 0 {
        return 0.0;
    }
    let max_hp = mon.stats[0];
    let hp_fraction = if max_hp == 0 {
        // Not reachable through normal team construction; scoring it as healthy
        // beats dividing by zero.
        1.0
    } else {
        (f64::from(mon.hp) / f64::from(max_hp)).clamp(0.0, 1.0)
    };
    ALIVE_SHARE + (1.0 - ALIVE_SHARE) * hp_fraction
}

/// Returns the score cost of a nonvolatile status.
/// A fainted Pokémon carries no cost, because it has no score left to lose.
/// Status stays relevant on the bench.
fn status_penalty(mon: &PokemonState) -> f64 {
    if mon.fainted || mon.hp == 0 {
        return 0.0;
    }
    match mon.status.as_ref() {
        None => 0.0,
        Some(Status::Sleep(_)) => 0.35,
        Some(Status::Frozen(_)) => 0.35,
        Some(Status::Paralysis) => 0.22,
        Some(Status::Burn) => 0.18,
        // Toxic accelerates, so it is worth strictly more than regular poison,
        // and more so the longer it has been ticking.
        Some(Status::ToxicPoison(turns)) => 0.15 + 0.02 * f64::from(*turns).min(8.0),
        Some(Status::Poison) => 0.12,
    }
}

/// Entry hazards on a side, charged to that side. Layered hazards scale with
/// their layer count; Stealth Rock and Sticky Web are single-layer.
fn hazard_penalty(side_conditions: &[SideCondition]) -> f64 {
    side_conditions
        .iter()
        .map(|condition| match condition {
            SideCondition::Spikes(layers) => HAZARD_WEIGHT * f64::from(*layers),
            SideCondition::ToxicSpikes(layers) => HAZARD_WEIGHT * f64::from(*layers),
            SideCondition::StealthRock => HAZARD_WEIGHT * 1.5,
            SideCondition::StickyWeb(_) => HAZARD_WEIGHT,
            _ => 0.0,
        })
        .sum()
}

/// Damage-reducing screens standing on a side.
fn screen_count(side_conditions: &[SideCondition]) -> f64 {
    side_conditions
        .iter()
        .filter(|condition| {
            matches!(
                condition,
                SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil
            )
        })
        .count() as f64
}

/// How reliably one Pokémon can still protect.
///
/// A protecting move succeeds with probability `1 / 3^stall_counter`, so the
/// feature decays with the same rule that the move itself uses.
fn protect_pressure(mon: &PokemonState, ctx: &EvalContext<'_>) -> f64 {
    let holds_protect = mon
        .moves
        .iter()
        .flatten()
        .filter_map(|name| ctx.move_dex.get(name))
        .any(|move_data| move_data.stalling_move);
    if !holds_protect {
        return 0.0;
    }
    1.0 / 3f64.powi(i32::from(mon.stall_counter))
}

/// The matchup features of one side, as
/// `(threat, guaranteed_kill, possible_kill, speed)`.
///
/// The evaluator visits each pair of one own active slot and one opposing
/// active slot. Targeting falls out of that loop: a side that threatens both
/// opposing slots scores both pairs.
///
/// Each attacker asks for its legal commands once, outside the defender loop.
/// That call is the largest single cost of one leaf.
fn matchup_features(
    state: &BattleState,
    player: Player,
    ctx: &EvalContext<'_>,
) -> (f64, f64, f64, f64) {
    let (attackers, defenders) = match player {
        Player::P1 => (&state.p1_active_mons, &state.p2_active_mons),
        Player::P2 => (&state.p2_active_mons, &state.p1_active_mons),
    };
    let trick_room = state.pseudo_weathers.contains(&PseudoWeather::TrickRoom);

    let mut threat = 0.0;
    let mut guaranteed = 0.0;
    let mut possible = 0.0;
    let mut speed = 0.0;

    for (attacker_index, attacker) in attackers.iter().enumerate() {
        if attacker.fainted || attacker.hp == 0 {
            continue;
        }
        let user_slot = FieldSlot {
            player,
            slot_index: attacker_index as u8,
        };
        let legal_commands = get_possible_commands_for_active_slot(
            state,
            player,
            attacker_index,
            ctx.move_dex,
            ctx.pokemon_dex,
        );
        for (defender_index, defender) in defenders.iter().enumerate() {
            if defender.fainted || defender.hp == 0 {
                continue;
            }
            let target_slot = FieldSlot {
                player: other_player(player),
                slot_index: defender_index as u8,
            };

            let own_speed = effective_speed_for_slot(state, user_slot, attacker);
            let their_speed = effective_speed_for_slot(state, target_slot, defender);
            let faster = if trick_room {
                own_speed < their_speed
            } else {
                own_speed > their_speed
            };
            if faster {
                speed += 1.0;
            }

            if let Some(best) = best_attack(
                state,
                attacker,
                defender,
                user_slot,
                target_slot,
                &legal_commands,
                ctx,
            ) {
                threat += best.expected_fraction;
                guaranteed += flag(best.guaranteed_kill);
                possible += best.kill_probability;
            }
        }
    }

    (threat, guaranteed, possible, speed)
}

/// What one attack does to one target.
#[derive(Debug, Clone, Copy)]
struct AttackValue {
    /// Expected damage as a fraction of the target's current HP, capped at one,
    /// times the hit probability.
    expected_fraction: f64,
    /// The chance that the attack removes the target this turn.
    kill_probability: f64,
    /// Whether every damage branch kills and the attack cannot miss.
    guaranteed_kill: bool,
}

/// The best of one attacker's four moves against one target.
///
/// "Best" means the largest [`AttackValue::expected_fraction`]. The kill terms
/// then describe that same move, so the three threat features always describe
/// one choice.
///
/// `legal_commands` holds the commands that the slot may still pick. A move
/// without PP, a disabled move, and a move that a Choice item locks away are
/// all absent from that list, so none of them scores as a threat.
///
/// The plain form of each move supplies the estimate. Terastallization and Mega
/// Evolution change the user, and neither is free, so the threat features price
/// the move that the attacker can use without spending a resource.
///
/// Returns `None` when the attacker has no damaging move that it may pick.
fn best_attack(
    state: &BattleState,
    attacker: &PokemonState,
    defender: &PokemonState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
    legal_commands: &[BattleCommand],
    ctx: &EvalContext<'_>,
) -> Option<AttackValue> {
    let mut best: Option<AttackValue> = None;
    for (move_slot, name) in attacker.moves.iter().enumerate().filter_map(|(slot, name)| {
        name.as_ref().map(|name| (slot, name))
    }) {
        let selectable = legal_commands.iter().any(|command| {
            let BattleCommand::Attack(attack) = command else {
                return false;
            };
            attack.move_slot == move_slot
                && !attack.terastallize
                && !attack.mega_evolve
                && attack.target.is_none_or(|target| target == target_slot)
        });
        if !selectable {
            continue;
        }
        let Some(move_data) = ctx.move_dex.get(name) else {
            continue;
        };
        if matches!(move_data.category, MoveCategory::Status) {
            continue;
        }
        let Some(candidate) =
            single_move_value(state, attacker, defender, user_slot, target_slot, move_data)
        else {
            continue;
        };
        if best.is_none_or(|current| candidate.expected_fraction > current.expected_fraction) {
            best = Some(candidate);
        }
    }
    best
}

/// The opposing player.
fn other_player(player: Player) -> Player {
    match player {
        Player::P1 => Player::P2,
        Player::P2 => Player::P1,
    }
}

/// Turns a flag into a feature value.
fn flag(set: bool) -> f64 {
    if set { 1.0 } else { 0.0 }
}

/// Map a signed advantage in Pokemon-equivalents onto `(0, 1)`.
pub fn logistic(advantage: f64) -> f64 {
    1.0 / (1.0 + (-LOGISTIC_SCALE * advantage).exp())
}

// ── Stored weights ──────────────────────────────────────────────────────────

/// A named weight vector, as `bin/train_eval` writes it.
///
/// The names travel with the values, so a feature-order change cannot silently
/// reassign a weight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Weights {
    /// Feature names, in the order of `values`.
    pub names: Vec<String>,
    /// One weight for each name.
    pub values: Vec<f64>,
}

impl Weights {
    /// Builds a record from an array and a name list.
    pub fn from_array(names: &[&str], values: &[f64]) -> Weights {
        Weights {
            names: names.iter().map(|name| name.to_string()).collect(),
            values: values.to_vec(),
        }
    }

    /// Reads the weight of one feature.
    pub fn get(&self, name: &str) -> Option<f64> {
        let index = self.names.iter().position(|stored| stored == name)?;
        self.values.get(index).copied()
    }
}

/// The trained value weights, as JSON.
const EVAL_WEIGHTS_JSON: &str = include_str!("../../weights/eval_v1.json");

/// The trained policy weights, as JSON.
const POLICY_WEIGHTS_JSON: &str = include_str!("../../weights/policy_v1.json");

/// Returns the trained value weights.
///
/// Parsing runs once. A file that fails to parse falls back to
/// [`HAND_WEIGHTS`], because a leaf evaluator must not panic mid-search.
pub fn fitted_weights() -> &'static Features {
    static CACHE: OnceLock<Features> = OnceLock::new();
    CACHE.get_or_init(|| resolve(EVAL_WEIGHTS_JSON, &FEATURE_NAMES, &HAND_WEIGHTS))
}

/// Returns the trained policy weights.
pub fn fitted_policy_weights() -> &'static PolicyFeatures {
    static CACHE: OnceLock<PolicyFeatures> = OnceLock::new();
    CACHE.get_or_init(|| {
        resolve(
            POLICY_WEIGHTS_JSON,
            &POLICY_FEATURE_NAMES,
            &HAND_POLICY_WEIGHTS,
        )
    })
}

/// Fills a weight array from JSON, one name at a time.
/// A missing name keeps its fallback value.
fn resolve<const N: usize>(json: &str, names: &[&str; N], fallback: &[f64; N]) -> [f64; N] {
    let mut out = *fallback;
    let Ok(stored) = serde_json::from_str::<Weights>(json) else {
        return out;
    };
    for (index, name) in names.iter().enumerate() {
        if let Some(value) = stored.get(name)
            && value.is_finite()
        {
            out[index] = value;
        }
    }
    out
}

// ── The action policy ───────────────────────────────────────────────────────

/// How many features the policy model holds.
pub const POLICY_FEATURE_COUNT: usize = 8;

/// Names the policy features in index order.
pub const POLICY_FEATURE_NAMES: [&str; POLICY_FEATURE_COUNT] = [
    "damage",
    "kill",
    "accuracy",
    "priority",
    "faster",
    "switch",
    "protect",
    "status_move",
];

/// One value for each policy feature.
pub type PolicyFeatures = [f64; POLICY_FEATURE_COUNT];

/// The hand-set policy weights.
pub const HAND_POLICY_WEIGHTS: PolicyFeatures = [
    2.0,  // damage
    3.0,  // kill
    0.5,  // accuracy
    0.3,  // priority
    0.3,  // faster
    -0.2, // switch
    0.1,  // protect
    -0.3, // status_move
];

/// The features of one joint action.
///
/// A joint action holds one command for each active slot, so each feature sums
/// over the slots.
pub fn policy_features(
    state: &BattleState,
    player: Player,
    action: &[BattleCommand],
    ctx: &EvalContext<'_>,
) -> PolicyFeatures {
    let attackers = match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    let trick_room = state.pseudo_weathers.contains(&PseudoWeather::TrickRoom);

    let mut out = [0.0; POLICY_FEATURE_COUNT];
    for (slot_index, command) in action.iter().enumerate() {
        match command {
            BattleCommand::Switch(_) => out[5] += 1.0,
            BattleCommand::Attack(attack) => {
                let Some(attacker) = attackers.get(slot_index) else {
                    continue;
                };
                let Some(name) = attacker.moves.get(attack.move_slot).and_then(|m| m.as_ref())
                else {
                    continue;
                };
                let Some(move_data) = ctx.move_dex.get(name) else {
                    continue;
                };
                if move_data.stalling_move {
                    out[6] += 1.0;
                }
                if matches!(move_data.category, MoveCategory::Status) {
                    out[7] += 1.0;
                    continue;
                }

                let user_slot = FieldSlot {
                    player,
                    slot_index: slot_index as u8,
                };
                let Some(target_slot) = attack
                    .target
                    .or_else(|| default_target(state, player))
                    .filter(|slot| slot.player != player)
                else {
                    continue;
                };
                let Some(defender) = active_at(state, target_slot) else {
                    continue;
                };
                if defender.fainted || defender.hp == 0 {
                    continue;
                }

                let mut transformed;
                let attacker = if attack.terastallize || attack.mega_evolve {
                    transformed = attacker.clone();
                    if attack.mega_evolve {
                        try_mega_evolution(&mut transformed, ctx.pokemon_dex);
                    }
                    if attack.terastallize {
                        transformed.is_tera = true;
                    }
                    &transformed
                } else {
                    attacker
                };

                if let Some(value) =
                    single_move_value(state, attacker, defender, user_slot, target_slot, move_data)
                {
                    out[0] += value.expected_fraction;
                    out[1] += value.kill_probability;
                    out[2] += accuracy_hit_probability(
                        state,
                        attacker,
                        defender,
                        user_slot,
                        target_slot,
                        move_data,
                    );
                }
                out[3] += f64::from(effective_move_priority(state, attacker, move_data));

                let own_speed = effective_speed_for_slot(state, user_slot, attacker);
                let their_speed = effective_speed_for_slot(state, target_slot, defender);
                let faster = if trick_room {
                    own_speed < their_speed
                } else {
                    own_speed > their_speed
                };
                if faster {
                    out[4] += 1.0;
                }
            }
            BattleCommand::Struggle { .. } | BattleCommand::Pass => {}
        }
    }
    out
}

/// The score of one joint action, before the softmax.
pub fn policy_score(
    state: &BattleState,
    player: Player,
    action: &[BattleCommand],
    ctx: &EvalContext<'_>,
    weights: &PolicyFeatures,
) -> f64 {
    policy_features(state, player, action, ctx)
        .iter()
        .zip(weights.iter())
        .map(|(value, weight)| value * weight)
        .sum()
}

/// A softmax over one player's joint actions.
///
/// The result holds one probability for each action, and the probabilities sum
/// to one. An empty action list returns an empty vector.
pub fn policy_scores(
    state: &BattleState,
    player: Player,
    actions: &[Vec<BattleCommand>],
    ctx: &EvalContext<'_>,
    weights: &PolicyFeatures,
) -> Vec<f64> {
    let raw: Vec<f64> = actions
        .iter()
        .map(|action| policy_score(state, player, action, ctx, weights))
        .collect();
    softmax(&raw)
}

/// Normalizes scores into a probability distribution.
///
/// The largest score is subtracted first, so a large score cannot overflow the
/// exponential.
pub fn softmax(scores: &[f64]) -> Vec<f64> {
    if scores.is_empty() {
        return Vec::new();
    }
    if scores.iter().any(|score| !score.is_finite()) {
        let uniform = 1.0 / scores.len() as f64;
        return vec![uniform; scores.len()];
    }
    let largest = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exponentials: Vec<f64> = scores
        .iter()
        .map(|score| (score - largest).exp())
        .collect();
    let total: f64 = exponentials.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        let uniform = 1.0 / scores.len() as f64;
        return vec![uniform; scores.len()];
    }
    exponentials.iter().map(|value| value / total).collect()
}

/// The first healthy opposing active slot.
/// A singles command carries no target, so the policy resolves one here.
fn default_target(state: &BattleState, player: Player) -> Option<FieldSlot> {
    let defenders = match player {
        Player::P1 => &state.p2_active_mons,
        Player::P2 => &state.p1_active_mons,
    };
    defenders
        .iter()
        .position(|mon| !mon.fainted && mon.hp > 0)
        .map(|index| FieldSlot {
            player: other_player(player),
            slot_index: index as u8,
        })
}

/// Reads the active Pokémon of one slot.
fn active_at(state: &BattleState, slot: FieldSlot) -> Option<&PokemonState> {
    let mons = match slot.player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    mons.get(slot.slot_index as usize)
}

/// What one named move does to one target.
/// [`best_attack`] runs the same calculation over every move slot.
fn single_move_value(
    state: &BattleState,
    attacker: &PokemonState,
    defender: &PokemonState,
    user_slot: FieldSlot,
    target_slot: FieldSlot,
    move_data: &MoveData,
) -> Option<AttackValue> {
    let target_hp = defender.hp;
    if target_hp == 0 {
        return None;
    }
    let outcomes = calculate_damage_outcomes_for_target(
        state,
        attacker,
        defender,
        user_slot,
        target_slot,
        move_data,
        EVAL_DAMAGE_CONFIG,
        1.0,
        1.0,
    );
    let mass: f64 = outcomes.iter().map(|(_, _, probability)| probability).sum();
    if mass <= 0.0 {
        return None;
    }
    let expected_damage: f64 = outcomes
        .iter()
        .map(|(damage, _, probability)| f64::from(*damage) * probability)
        .sum::<f64>()
        / mass;
    let kill_mass: f64 = outcomes
        .iter()
        .filter(|(damage, _, _)| *damage >= target_hp)
        .map(|(_, _, probability)| probability)
        .sum::<f64>()
        / mass;
    let hit_probability =
        accuracy_hit_probability(state, attacker, defender, user_slot, target_slot, move_data);
    let fraction = (expected_damage / f64::from(target_hp)).clamp(0.0, 1.0);
    Some(AttackValue {
        expected_fraction: fraction * hit_probability,
        kill_probability: kill_mass * hit_probability,
        guaranteed_kill: kill_mass >= 1.0 && hit_probability >= 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::pokemon_move::PokemonMove;
    use crate::data::species::Species;
    use crate::state::dex_data::{MoveData, PokemonData};
    use crate::state::pokemon::build_pokemon_state;
    use crate::tests::simuilator_test_helpers::{battle_state_from_lists, move_dex, pokemon_dex};
    use std::collections::HashMap;

    fn ctx() -> EvalContext<'static> {
        EvalContext::new(pokemon_dex(), move_dex())
    }

    fn mon(
        species: Species,
        pokemon_dex: &HashMap<Species, PokemonData>,
        move_dex: &HashMap<PokemonMove, MoveData>,
    ) -> PokemonState {
        build_pokemon_state(
            species,
            pokemon_dex,
            move_dex,
            Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
    }

    #[test]
    fn mirrored_position_is_even() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
        );
        assert!((heuristic(&state, &ctx()) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn losing_hp_lowers_the_score() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
        );
        let even_score = heuristic(&state, &ctx());
        state.p1_active_mons[0].hp /= 2;
        assert!(heuristic(&state, &ctx()) < even_score);
    }

    #[test]
    fn a_faint_costs_more_than_any_chip_damage() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let build = || {
            battle_state_from_lists(
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            )
        };

        let mut chipped = build();
        chipped.p1_back_mons[0].hp = 1;

        let mut fainted = build();
        fainted.p1_back_mons[0].hp = 0;
        fainted.p1_back_mons[0].fainted = true;

        assert!(heuristic(&fainted, &ctx()) < heuristic(&chipped, &ctx()));
    }

    #[test]
    fn status_and_hazards_are_charged_to_the_afflicted_side() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let build = || {
            battle_state_from_lists(
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![],
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![],
            )
        };

        let mut burned = build();
        burned.p1_active_mons[0].status = Some(Status::Burn);
        assert!(heuristic(&burned, &ctx()) < 0.5);

        // Status survives switching, so it has to be charged on the bench too.
        let mut benched_burn = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
        );
        benched_burn.p1_back_mons[0].status = Some(Status::Burn);
        assert!(heuristic(&benched_burn, &ctx()) < 0.5);

        let mut hazards = build();
        hazards.p1_side_conditions.push(SideCondition::StealthRock);
        assert!(heuristic(&hazards, &ctx()) < 0.5);

        // Same hazard on P2's side must swing the other way by the same amount.
        let mut theirs = build();
        theirs.p2_side_conditions.push(SideCondition::StealthRock);
        assert!((heuristic(&theirs, &ctx()) + heuristic(&hazards, &ctx()) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn output_stays_in_range_under_a_total_wipe() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
        );
        for m in state
            .p2_active_mons
            .iter_mut()
            .chain(state.p2_back_mons.iter_mut())
        {
            m.hp = 0;
            m.fainted = true;
        }
        let score = heuristic(&state, &ctx());
        assert!((0.0..=1.0).contains(&score), "out of range: {score}");
        assert!(score > 0.8, "a wipe should read as winning: {score}");
    }

    #[test]
    fn softmax_sums_to_one() {
        let scores = softmax(&[1.0, 2.0, 3.0]);
        let total: f64 = scores.iter().sum();
        assert!((total - 1.0).abs() < 1e-12);
        assert!(scores[2] > scores[1] && scores[1] > scores[0]);
    }

    #[test]
    fn softmax_returns_a_distribution_for_nonfinite_input() {
        for input in [[f64::NAN, 1.0], [f64::INFINITY, 1.0]] {
            let scores = softmax(&input);
            assert_eq!(scores, vec![0.5, 0.5]);
            assert!(scores.iter().all(|score| score.is_finite()));
        }
    }
}
