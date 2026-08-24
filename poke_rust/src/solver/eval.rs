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
//! # Field state
//!
//! Weather, terrain, and a pseudo-weather belong to the field, so both sides
//! read the same value. A raw indicator of one of them holds the same value on
//! both sides and subtracts to zero.
//!
//! Each field feature therefore counts what one side gains from the field, not
//! whether the field is up. [`weather_features`] and [`terrain_features`] hold
//! that rule, and [`matchup_features`] holds it for Trick Room.
//!
//! A side condition is already stored per side, so it needs no re-expression.
//!
//! # The bench
//!
//! A benched Pokemon scores through `health` and `status` alone in the first
//! twenty features. Champions doubles brings four Pokemon and leads two, so
//! half of each team would otherwise score as a health bar.
//!
//! [`bench_features`] adds three values that read the bench:
//!
//! - `bench_threat` — what the best switch-in does to the opposing actives.
//! - `switch_in_damage` — what the opposing actives do to that switch-in.
//! - `team_coverage` — the type reach of the whole living team.
//!
//! [`best_switch_in`] picks one Pokemon with a type-chart proxy, and the damage
//! calculation then runs for that Pokemon alone. Bench size therefore does not
//! multiply the expensive work.
//!
//! A side with no living bench has no switch-in. `bench_threat` then reads 0,
//! which is the smallest value that any bench can produce. `switch_in_damage`
//! reads [`NO_SWITCH_IN_DAMAGE`] for each living opposing active, which is the
//! largest value that any bench can produce. Both are the worst value of their
//! own column, so an empty bench never scores above an occupied one.
//!
//! # Three scorers
//!
//! [`heuristic`] uses the hand-set weights in [`HAND_WEIGHTS`].
//! [`fitted`] uses the linear weights that `bin/train_eval` produced.
//! [`fitted_mlp`] uses the network that the same binary produced.
//! `weights/eval_v1.json` and `weights/eval_mlp_v1.json` hold the two fits.
//!
//! [`Mlp`] carries no bias term, and its activation is odd, so the network keeps
//! side-swap symmetry for every weight matrix.
//!
//! `src/solver/TRAINING.md` holds the rerun procedure.
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

use crate::data::ability::Ability;
use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::unknowns::UnknownBattleState;
use crate::simulator::{DamageConfig, get_possible_commands_for_active_slot};
use crate::simulator::helpers::{
    accuracy_hit_probability, calculate_damage_outcomes_for_target, current_terrain,
    current_weather, effective_move_priority, effective_move_type, effective_speed_for_slot,
    move_type_effectiveness_with_attacker, pokemon_is_grounded,
};
use crate::state::battle::{
    BattleCommand, BattleState, FieldSlot, Player, try_mega_evolution,
};
use crate::state::dex_data::{
    MoveCategory, MoveData, PokemonData, PokemonType, PseudoWeather, SideCondition, Status,
    Terrain, Weather,
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
pub const FEATURE_COUNT: usize = 23;

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
    "weather_edge",
    "weather_control",
    "terrain_edge",
    "terrain_control",
    "tailwind",
    "guard_conditions",
    "trick_room",
    "bench_threat",
    "switch_in_damage",
    "team_coverage",
];

/// One value for each feature, in the order of [`FEATURE_NAMES`].
pub type Features = [f64; FEATURE_COUNT];

/// The hand-set weights of [`heuristic`].
///
/// The first five entries reproduce the health, status, boost, and hazard terms
/// of the original evaluator.
/// The next eight are initial values for the matchup features.
/// The next seven are initial values for the field and side-condition features.
/// The last three are initial values for the bench features.
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
    0.12, // weather_edge
    0.08, // weather_control
    0.10, // terrain_edge
    0.07, // terrain_control
    0.14, // tailwind
    0.05, // guard_conditions
    0.06, // trick_room
    0.10, // bench_threat
    -0.10, // switch_in_damage
    0.08, // team_coverage
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

/// Damage rolls of the threat features.
///
/// One roll makes the kill mass zero or one, so `possible_kill` equals
/// `guaranteed_kill` on every move that cannot miss. The two features are then
/// collinear, and no corpus can weight them apart.
///
/// The damage function computes every multiplier once and then loops the rolls,
/// so the extra rolls add cheap steps rather than whole calculations.
/// `cargo bench --bench solver_speed -- --leaf-cost` measures the result.
pub const EVAL_DAMAGE_ROLLS: u8 = 16;

/// The damage settings of the threat features.
///
/// The critical-hit branch stays out, because the evaluator compares a matchup
/// instead of two moves of one slot.
const EVAL_DAMAGE_CONFIG: DamageConfig = DamageConfig {
    consider_crit: false,
    damage_rolls: EVAL_DAMAGE_ROLLS,
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
    let commands = ActiveCommands::build(state, ctx);
    let p1 = side_features(state, Player::P1, ctx, &commands);
    let p2 = side_features(state, Player::P2, ctx, &commands);
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
fn side_features(
    state: &BattleState,
    player: Player,
    ctx: &EvalContext<'_>,
    commands: &ActiveCommands,
) -> Features {
    let (active, bench, side_conditions, condition_turns, has_tera, has_mega) = match player {
        Player::P1 => (
            &state.p1_active_mons,
            &state.p1_back_mons,
            &state.p1_side_conditions,
            &state.p1_side_condition_turns,
            state.p1_has_tera,
            state.p1_has_mega,
        ),
        Player::P2 => (
            &state.p2_active_mons,
            &state.p2_back_mons,
            &state.p2_side_conditions,
            &state.p2_side_condition_turns,
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

    let (weather_edge, weather_control) = weather_features(state, active, bench);
    out[13] = weather_edge;
    out[14] = weather_control;
    let (terrain_edge, terrain_control) = terrain_features(state, active, bench);
    out[15] = terrain_edge;
    out[16] = terrain_control;
    out[17] = timed_condition(side_conditions, condition_turns, &SideCondition::TailWind);
    out[18] = guard_conditions(side_conditions, condition_turns);

    let (threat, guaranteed, possible, speed, trick_room) =
        matchup_features(state, player, ctx, commands);
    out[5] = threat;
    out[6] = guaranteed;
    out[7] = possible;
    out[8] = speed;
    out[19] = trick_room;

    let (bench_threat, switch_in_damage, team_coverage) =
        bench_features(state, player, ctx, commands);
    out[20] = bench_threat;
    out[21] = switch_in_damage;
    out[22] = team_coverage;
    out
}

/// Scores one Pokémon from health alone.
fn alive_score(mon: &PokemonState) -> f64 {
    if !is_alive(mon) {
        return 0.0;
    }
    ALIVE_SHARE + (1.0 - ALIVE_SHARE) * hp_fraction(mon)
}

/// Whether one Pokémon can still act.
fn is_alive(mon: &PokemonState) -> bool {
    !mon.fainted && mon.hp > 0
}

/// Remaining HP as a fraction of maximum HP, from zero through one.
fn hp_fraction(mon: &PokemonState) -> f64 {
    let max_hp = mon.stats[0];
    if max_hp == 0 {
        // Not reachable through normal team construction; scoring it as healthy
        // beats dividing by zero.
        return 1.0;
    }
    (f64::from(mon.hp) / f64::from(max_hp)).clamp(0.0, 1.0)
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

// ── Field and side conditions ───────────────────────────────────────────

/// Turns that a timed effect is worth at full value.
///
/// Every timer in the engine counts down, so an effect with one turn left is
/// nearly spent. Five turns is the standard duration of weather, terrain,
/// Tailwind, and Trick Room, so a fresh effect scores one.
const EFFECT_DURATION_SCALE: f64 = 5.0;

/// How much a bench Pokémon counts against an active one.
///
/// A bench Pokémon cannot use the field this turn. The field is still standing
/// when it comes in, so it is worth more than nothing.
const BENCH_SHARE: f64 = 0.25;

/// How much an unspent setter counts against a setter whose field is already up.
const SETTER_SHARE: f64 = 0.5;

/// Scales an effect by the turns it has left.
///
/// Zero turns means that the effect carries no timer. A side condition stores
/// zero for a standing effect, and weather stores `None` for the same case, so
/// both read as full value.
fn duration_scale(turns: u8) -> f64 {
    if turns == 0 {
        return 1.0;
    }
    f64::from(turns).min(EFFECT_DURATION_SCALE) / EFFECT_DURATION_SCALE
}

/// One named side condition, scaled by the turns it has left.
fn timed_condition(conditions: &[SideCondition], turns: &[u8], wanted: &SideCondition) -> f64 {
    conditions
        .iter()
        .zip(turns.iter())
        .find(|(condition, _)| *condition == wanted)
        .map_or(0.0, |(_, left)| duration_scale(*left))
}

/// Status-blocking side conditions standing on a side.
///
/// Safeguard, Mist, and Lucky Chant each refuse an effect that the opponent
/// wants to apply. None of them appear in [`screen_count`], which reads the
/// damage-reducing screens alone.
fn guard_conditions(conditions: &[SideCondition], turns: &[u8]) -> f64 {
    conditions
        .iter()
        .zip(turns.iter())
        .filter(|(condition, _)| {
            matches!(
                condition,
                SideCondition::SafeGuard | SideCondition::Mist | SideCondition::LuckyChant
            )
        })
        .map(|(_, left)| duration_scale(*left))
        .sum()
}

/// The weather that one ability sets on entry.
fn weather_of_setter(ability: &Ability) -> Option<Weather> {
    match ability {
        Ability::Drizzle => Some(Weather::Rain),
        Ability::PrimordialSea => Some(Weather::HeavyRain),
        Ability::Drought | Ability::OrichalcumPulse => Some(Weather::Sun),
        Ability::DesolateLand => Some(Weather::ExtremeSunlight),
        Ability::SandStream => Some(Weather::Sandstorm),
        Ability::SnowWarning => Some(Weather::Snow),
        Ability::DeltaStream => Some(Weather::StrongWinds),
        _ => None,
    }
}

/// The terrain that one ability sets on entry.
fn terrain_of_setter(ability: &Ability) -> Option<Terrain> {
    match ability {
        Ability::ElectricSurge | Ability::HadronEngine => Some(Terrain::ElectricTerrain),
        Ability::GrassySurge => Some(Terrain::GrassyTerrain),
        Ability::MistySurge => Some(Terrain::MistyTerrain),
        Ability::PsychicSurge => Some(Terrain::PsychicTerrain),
        _ => None,
    }
}

/// What one Pokémon gains from the weather that is up now.
///
/// A negative value means that the weather hurts this Pokémon. Rain and sun
/// each help one attacking type and hurt the other. Sand chips the Pokémon that
/// do not resist it. Every weather also has abilities that turn it into a speed
/// advantage or a recovery advantage.
fn weather_benefit(mon: &PokemonState, weather: &Weather) -> f64 {
    let has_type = |wanted: PokemonType| mon.types.contains(&wanted);
    match weather {
        Weather::Rain | Weather::HeavyRain => {
            if matches!(
                mon.ability,
                Ability::SwiftSwim | Ability::RainDish | Ability::DrySkin | Ability::Hydration
            ) {
                return 1.0;
            }
            f64::from(has_type(PokemonType::Water)) * 0.5
                - f64::from(has_type(PokemonType::Fire)) * 0.5
        }
        Weather::Sun | Weather::ExtremeSunlight => {
            if matches!(
                mon.ability,
                Ability::Chlorophyll
                    | Ability::SolarPower
                    | Ability::FlowerGift
                    | Ability::LeafGuard
            ) {
                return 1.0;
            }
            let dry_skin = f64::from(mon.ability == Ability::DrySkin) * 0.5;
            f64::from(has_type(PokemonType::Fire)) * 0.5
                - f64::from(has_type(PokemonType::Water)) * 0.5
                - dry_skin
        }
        Weather::Sandstorm => {
            if matches!(
                mon.ability,
                Ability::SandRush | Ability::SandForce | Ability::SandVeil
            ) {
                return 1.0;
            }
            // Rock takes no chip and gains the special-defense boost. Ground and
            // Steel take no chip. Two abilities also stop the chip.
            if has_type(PokemonType::Rock) {
                return 0.75;
            }
            if has_type(PokemonType::Ground)
                || has_type(PokemonType::Steel)
                || matches!(mon.ability, Ability::Overcoat | Ability::MagicGuard)
            {
                return 0.25;
            }
            -0.25
        }
        Weather::Snow => {
            if matches!(
                mon.ability,
                Ability::SlushRush | Ability::IceBody | Ability::SnowCloak
            ) {
                return 1.0;
            }
            // Snow raises the defense of an Ice-type rather than chipping others.
            f64::from(has_type(PokemonType::Ice)) * 0.5
        }
        // Strong Winds removes the Flying weaknesses of a Flying-type.
        Weather::StrongWinds => f64::from(has_type(PokemonType::Flying)) * 0.5,
    }
}

/// What one grounded Pokémon gains from the terrain that is up now.
///
/// Terrain reaches a grounded Pokémon alone, so the caller checks that first.
/// Each terrain gives every grounded Pokémon a defensive effect. It also gives
/// one type an offensive boost.
fn terrain_benefit(mon: &PokemonState, terrain: &Terrain) -> f64 {
    let has_type = |wanted: PokemonType| mon.types.contains(&wanted);
    match terrain {
        Terrain::ElectricTerrain => {
            let surfer = f64::from(mon.ability == Ability::SurgeSurfer) * 0.5;
            0.25 + f64::from(has_type(PokemonType::Electric)) * 0.5 + surfer
        }
        Terrain::GrassyTerrain => {
            let pelt = f64::from(mon.ability == Ability::GrassPelt) * 0.25;
            0.35 + f64::from(has_type(PokemonType::Grass)) * 0.5 + pelt
        }
        Terrain::MistyTerrain => 0.35 + f64::from(has_type(PokemonType::Dragon)) * 0.25,
        Terrain::PsychicTerrain => 0.30 + f64::from(has_type(PokemonType::Psychic)) * 0.5,
    }
}

/// The weather edge of one side, and its control of the weather.
///
/// The first value counts what the side gains from the weather that is up now,
/// scaled by the turns that weather has left. The second value counts the living
/// weather setters of the side. It adds a bonus when the weather up now is the
/// weather that this side sets.
///
/// Weather is field state, so both sides read the same weather. A raw indicator
/// would hold the same value on both sides and would subtract to zero in
/// [`features`]. Only a side-relative count carries information.
fn weather_features(
    state: &BattleState,
    active: &[PokemonState],
    bench: &[PokemonState],
) -> (f64, f64) {
    let weather = current_weather(state);
    let scale = duration_scale(state.weather_turns.unwrap_or(0));

    let mut edge = 0.0;
    let mut control = 0.0;
    for (mon, share) in active
        .iter()
        .map(|mon| (mon, 1.0))
        .chain(bench.iter().map(|mon| (mon, BENCH_SHARE)))
    {
        if mon.fainted || mon.hp == 0 {
            continue;
        }
        if let Some(weather) = weather.as_ref() {
            edge += share * scale * weather_benefit(mon, weather);
        }
        if let Some(set) = weather_of_setter(&mon.ability) {
            // A setter that has not fired is a standing option, so it counts for
            // less than a setter whose weather already governs the field.
            control += share * SETTER_SHARE;
            if weather.as_ref() == Some(&set) {
                control += share * (1.0 - SETTER_SHARE);
            }
        }
    }
    (edge, control)
}

/// The terrain edge of one side, and its control of the terrain.
///
/// This is the weather pair with one extra rule: terrain reaches a grounded
/// Pokémon alone. A bench Pokémon has no ground state yet, so it counts through
/// [`BENCH_SHARE`] without the check.
fn terrain_features(
    state: &BattleState,
    active: &[PokemonState],
    bench: &[PokemonState],
) -> (f64, f64) {
    let terrain = current_terrain(state);
    let scale = duration_scale(state.terrain_turns.unwrap_or(0));

    let mut edge = 0.0;
    let mut control = 0.0;
    for (mon, share, grounded) in active
        .iter()
        .map(|mon| (mon, 1.0, pokemon_is_grounded(state, mon)))
        .chain(bench.iter().map(|mon| (mon, BENCH_SHARE, true)))
    {
        if mon.fainted || mon.hp == 0 {
            continue;
        }
        if let Some(terrain) = terrain.as_ref()
            && grounded
        {
            edge += share * scale * terrain_benefit(mon, terrain);
        }
        if let Some(set) = terrain_of_setter(&mon.ability) {
            control += share * SETTER_SHARE;
            if terrain.as_ref() == Some(&set) {
                control += share * (1.0 - SETTER_SHARE);
            }
        }
    }
    (edge, control)
}

/// The turns that Trick Room has left, or `None` when it is not up.
fn trick_room_turns(state: &BattleState) -> Option<u8> {
    state
        .pseudo_weathers
        .iter()
        .position(|pseudo| *pseudo == PseudoWeather::TrickRoom)
        .map(|index| state.pseudo_weather_turns.get(index).copied().unwrap_or(0))
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
/// [`ActiveCommands`] holds the legal commands of each attacker. That call is
/// the largest single cost of one leaf, so [`features`] makes it one time for
/// all four active slots.
fn matchup_features(
    state: &BattleState,
    player: Player,
    ctx: &EvalContext<'_>,
    commands: &ActiveCommands,
) -> (f64, f64, f64, f64, f64) {
    let (attackers, defenders) = match player {
        Player::P1 => (&state.p1_active_mons, &state.p2_active_mons),
        Player::P2 => (&state.p2_active_mons, &state.p1_active_mons),
    };
    let room_turns = trick_room_turns(state);
    let trick_room = room_turns.is_some();
    // `speed` already reads the reversed order, so this scale prices the part
    // that `speed` cannot see: how long the reversal has left to run.
    let room_scale = room_turns.map_or(0.0, duration_scale);

    let mut threat = 0.0;
    let mut guaranteed = 0.0;
    let mut possible = 0.0;
    let mut speed = 0.0;
    let mut room_edge = 0.0;

    for (attacker_index, attacker) in attackers.iter().enumerate() {
        if attacker.fainted || attacker.hp == 0 {
            continue;
        }
        let user_slot = FieldSlot {
            player,
            slot_index: attacker_index as u8,
        };
        let legal_commands = commands.of(player, attacker_index);
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
            // The side that the reversal favors is the slower side in raw speed.
            if trick_room && own_speed < their_speed {
                room_edge += room_scale;
            }

            if let Some(best) = best_attack(
                state,
                attacker,
                defender,
                user_slot,
                target_slot,
                MoveSelection::Commands {
                    commands: legal_commands,
                    match_target: true,
                },
                ctx,
            ) {
                threat += best.expected_fraction;
                guaranteed += flag(best.guaranteed_kill);
                possible += best.kill_probability;
            }
        }
    }

    (threat, guaranteed, possible, speed, room_edge)
}


// -- The bench --------------------------------------------------------------

/// The smallest type edge that [`type_edge`] returns, in log2 units.
///
/// A quarter multiplier reads this value. An immunity and an attacker with no
/// damaging move read it too, because neither can take HP away.
const TYPE_EDGE_FLOOR: f64 = -2.0;

/// The largest type edge that [`type_edge`] returns, in log2 units.
/// A quadruple multiplier reads this value.
const TYPE_EDGE_CEILING: f64 = 2.0;

/// The largest `switch_in_damage` that one opposing active can produce.
///
/// [`best_attack`] returns an `expected_fraction` from 0 through 1, so one
/// living opposing active adds at most this much.
///
/// A side with no living bench reads this value for each living opposing
/// active. The weight of the column is negative, so 0 is the best value of the
/// column and a side that cannot switch at all must not read it.
const NO_SWITCH_IN_DAMAGE: f64 = 1.0;

/// The bench features of one side, as
/// `(bench_threat, switch_in_damage, team_coverage)`.
///
/// [`best_switch_in`] names the one bench Pokemon that the first two values
/// describe. This follows the rule that [`best_attack`] already holds for the
/// three threat features: the values describe one choice, not a sum over every
/// choice.
///
/// A side with no living bench Pokemon reads the worst value of each of the
/// first two columns. `bench_threat` reads zero, and `switch_in_damage` reads
/// [`NO_SWITCH_IN_DAMAGE`] for each living opposing active.
///
/// Entry hazard damage stays out of `switch_in_damage`. The `hazards` feature
/// already holds it, and adding it twice would double its weight.
fn bench_features(
    state: &BattleState,
    player: Player,
    ctx: &EvalContext<'_>,
    commands: &ActiveCommands,
) -> (f64, f64, f64) {
    let coverage = team_coverage(state, player, ctx);
    let defenders = match player {
        Player::P1 => &state.p2_active_mons,
        Player::P2 => &state.p1_active_mons,
    };
    let Some(switch_in) = best_switch_in(state, player, ctx) else {
        let living = defenders.iter().filter(|mon| is_alive(mon)).count();
        return (0.0, NO_SWITCH_IN_DAMAGE * living as f64, coverage);
    };

    let entry_slot = entry_slot(state, player);
    let opponent = other_player(player);

    let mut threat = 0.0;
    let mut incoming = 0.0;
    for (defender_index, defender) in defenders.iter().enumerate() {
        if !is_alive(defender) {
            continue;
        }
        let defender_slot = FieldSlot {
            player: opponent,
            slot_index: defender_index as u8,
        };
        if let Some(best) = best_attack(
            state,
            switch_in,
            defender,
            entry_slot,
            defender_slot,
            MoveSelection::Bench,
            ctx,
        ) {
            threat += best.expected_fraction;
        }
        if let Some(best) = best_attack(
            state,
            defender,
            switch_in,
            defender_slot,
            entry_slot,
            MoveSelection::Commands {
                commands: commands.of(opponent, defender_index),
                match_target: false,
            },
            ctx,
        ) {
            incoming += best.expected_fraction;
        }
    }

    (threat, incoming, coverage)
}

/// The active slot that a switch-in enters.
///
/// The Pokemon is off the field, so it owns no slot, and no position says which
/// active it replaces. The damage call still reads a slot. `berry_env` reads the
/// occupant of the slot, `friend_guard_mult` and `has_plus_minus_partner` read
/// the other slots of the same side, and the screens read the whole side.
///
/// A slot index is a label alone. Exchanging the two active slots of a side must
/// not move a feature, and `solver_tests::slot_order_symmetry` asserts that rule
/// for the whole frame. A constant index breaks it: a Friend Guard ally counts
/// in one order and not in the other.
///
/// Every key of this choice therefore travels with the Pokemon and not with the
/// index. The slot is the one whose occupant is least able to stay in: a
/// fainted occupant first, then the smallest HP fraction, then the smallest
/// `mon_id`. `mon_id` is unique inside one team, so the order is total and the
/// chosen Pokemon follows an exchange into its new slot.
///
/// Returns the first slot when the side holds no active slot at all.
fn entry_slot(state: &BattleState, player: Player) -> FieldSlot {
    let active = match player {
        Player::P1 => &state.p1_active_mons,
        Player::P2 => &state.p2_active_mons,
    };
    let slot_index = active
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            is_alive(left)
                .cmp(&is_alive(right))
                .then_with(|| hp_fraction(left).total_cmp(&hp_fraction(right)))
                .then_with(|| left.mon_id.cmp(&right.mon_id))
        })
        .map_or(0, |(index, _)| index as u8);
    FieldSlot { player, slot_index }
}

/// The bench Pokemon that reads the best type-chart proxy against the opposing
/// actives.
///
/// The proxy is `coverage_out - coverage_in`, summed over the living opposing
/// actives. It costs type-chart reads alone, so bench size does not multiply
/// the damage calculation that [`bench_features`] then runs.
///
/// A larger HP fraction breaks a tie. The party index breaks a second tie.
///
/// Returns `None` when the side has no living bench Pokemon.
fn best_switch_in<'a>(
    state: &'a BattleState,
    player: Player,
    ctx: &EvalContext<'_>,
) -> Option<&'a PokemonState> {
    let (bench, defenders) = match player {
        Player::P1 => (&state.p1_back_mons, &state.p2_active_mons),
        Player::P2 => (&state.p2_back_mons, &state.p1_active_mons),
    };
    let mut best: Option<(&PokemonState, f64, f64)> = None;
    for mon in bench.iter().filter(|mon| is_alive(mon)) {
        let mut proxy = 0.0;
        for defender in defenders.iter().filter(|mon| is_alive(mon)) {
            proxy += type_edge(state, mon, defender, ctx);
            proxy -= type_edge(state, defender, mon, ctx);
        }
        let health = hp_fraction(mon);
        let better = match best {
            None => true,
            Some((_, stored_proxy, stored_health)) => {
                proxy > stored_proxy || (proxy == stored_proxy && health > stored_health)
            }
        };
        if better {
            best = Some((mon, proxy, health));
        }
    }
    best.map(|(mon, _, _)| mon)
}

/// The type reach of one living team against the other living team.
///
/// Each opposing Pokemon, active and benched, contributes the best type edge
/// that any living own Pokemon holds against it. The value is the mean over the
/// opposing Pokemon, so team size does not change its scale.
///
/// The value is zero when either side has no living Pokemon.
fn team_coverage(state: &BattleState, player: Player, ctx: &EvalContext<'_>) -> f64 {
    let (own_active, own_bench, their_active, their_bench) = match player {
        Player::P1 => (
            &state.p1_active_mons,
            &state.p1_back_mons,
            &state.p2_active_mons,
            &state.p2_back_mons,
        ),
        Player::P2 => (
            &state.p2_active_mons,
            &state.p2_back_mons,
            &state.p1_active_mons,
            &state.p1_back_mons,
        ),
    };
    let attackers: Vec<&PokemonState> = own_active
        .iter()
        .chain(own_bench.iter())
        .filter(|mon| is_alive(mon))
        .collect();
    if attackers.is_empty() {
        return 0.0;
    }

    let mut total = 0.0;
    let mut count = 0usize;
    for defender in their_active
        .iter()
        .chain(their_bench.iter())
        .filter(|mon| is_alive(mon))
    {
        let mut best = TYPE_EDGE_FLOOR;
        for attacker in attackers.iter() {
            let edge = type_edge(state, attacker, defender, ctx);
            if edge > best {
                best = edge;
            }
        }
        total += best;
        count += 1;
    }
    if count == 0 {
        return 0.0;
    }
    total / count as f64
}

/// The best type multiplier of one attacker against one defender, in log2 units.
///
/// The value is `log2(multiplier)`, clamped to the range [`TYPE_EDGE_FLOOR`]
/// through [`TYPE_EDGE_CEILING`]. Neutral therefore reads zero, and the map is
/// odd in the multiplier, so a resisted matchup and a super-effective one are
/// worth the same amount with opposite signs.
///
/// The reading is a proxy. It skips base power, the attacking stats, and PP, so
/// it costs the type chart alone.
///
/// The move type is the one that the damage calculation would use.
/// `effective_move_type` holds Weather Ball, Terrain Pulse, Aura Wheel, an -ate
/// ability, Liquid Voice, and Electrify, so a Pixilate Hyper Voice reads Fairy
/// here as well as in `single_move_value`. Reading `MoveData::pokemon_type`
/// instead would rate that attacker neutral against a Dragon-type defender.
fn type_edge(
    state: &BattleState,
    attacker: &PokemonState,
    defender: &PokemonState,
    ctx: &EvalContext<'_>,
) -> f64 {
    let mut best = TYPE_EDGE_FLOOR;
    for move_data in attacker
        .moves
        .iter()
        .flatten()
        .filter_map(|name| ctx.move_dex.get(name))
    {
        if matches!(move_data.category, MoveCategory::Status) {
            continue;
        }
        let multiplier = move_type_effectiveness_with_attacker(
            state,
            &effective_move_type(state, attacker, move_data),
            Some(attacker),
            defender,
        );
        let edge = multiplier.log2().clamp(TYPE_EDGE_FLOOR, TYPE_EDGE_CEILING);
        if edge > best {
            best = edge;
        }
    }
    best
}

/// Which move slots one attacker may pick.
///
/// An attacker that stands in an active slot reads a legal-command list. A move
/// without PP, a disabled move, and a move that a Choice item locks away are all
/// absent from that list, so none of them scores as a threat.
///
/// An attacker that sits on the bench has no slot and no list. A switch clears
/// Disable and a Choice lock, so every move that still holds PP is selectable.
#[derive(Debug, Clone, Copy)]
enum MoveSelection<'a> {
    /// The legal commands of one active slot.
    ///
    /// Set `match_target` when the target stands on the field. Clear it when the
    /// target sits on the bench, because no command names a bench slot and the
    /// check would then refuse every targeted move.
    Commands {
        commands: &'a [BattleCommand],
        match_target: bool,
    },
    /// The attacker sits on the bench.
    Bench,
}

impl MoveSelection<'_> {
    /// Whether the attacker may pick the move in `move_slot`.
    fn allows(&self, move_slot: usize, attacker: &PokemonState, target_slot: FieldSlot) -> bool {
        match self {
            MoveSelection::Commands {
                commands,
                match_target,
            } => commands.iter().any(|command| {
                let BattleCommand::Attack(attack) = command else {
                    return false;
                };
                attack.move_slot == move_slot
                    && !attack.terastallize
                    && !attack.mega_evolve
                    && (!*match_target
                        || attack.target.is_none_or(|target| target == target_slot))
            }),
            MoveSelection::Bench => attacker
                .move_pp
                .get(move_slot)
                .is_some_and(|left| *left > 0),
        }
    }
}

/// The legal commands of every active slot of one position.
///
/// [`matchup_features`] reads the list of each own active, and
/// [`bench_features`] reads the list of each opposing active. Both sides need
/// both lists, so [`features`] builds all four one time. The call count of one
/// leaf therefore stays at four.
///
/// A fainted slot holds an empty list. No feature reads it.
struct ActiveCommands {
    p1: Vec<Vec<BattleCommand>>,
    p2: Vec<Vec<BattleCommand>>,
}

impl ActiveCommands {
    /// Builds the list of each living active slot.
    fn build(state: &BattleState, ctx: &EvalContext<'_>) -> ActiveCommands {
        ActiveCommands {
            p1: Self::for_side(state, Player::P1, ctx),
            p2: Self::for_side(state, Player::P2, ctx),
        }
    }

    fn for_side(
        state: &BattleState,
        player: Player,
        ctx: &EvalContext<'_>,
    ) -> Vec<Vec<BattleCommand>> {
        let active = match player {
            Player::P1 => &state.p1_active_mons,
            Player::P2 => &state.p2_active_mons,
        };
        active
            .iter()
            .enumerate()
            .map(|(index, mon)| {
                if !is_alive(mon) {
                    return Vec::new();
                }
                get_possible_commands_for_active_slot(
                    state,
                    player,
                    index,
                    ctx.move_dex,
                    ctx.pokemon_dex,
                )
            })
            .collect()
    }

    /// The list of one slot, or an empty list when the slot does not exist.
    fn of(&self, player: Player, slot_index: usize) -> &[BattleCommand] {
        let side = match player {
            Player::P1 => &self.p1,
            Player::P2 => &self.p2,
        };
        side.get(slot_index).map_or(&[], |list| list.as_slice())
    }
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
/// `selection` decides which move slots the attacker may pick. Read
/// [`MoveSelection`] for the two cases.
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
    selection: MoveSelection<'_>,
    ctx: &EvalContext<'_>,
) -> Option<AttackValue> {
    let mut best: Option<AttackValue> = None;
    for (move_slot, name) in attacker.moves.iter().enumerate().filter_map(|(slot, name)| {
        name.as_ref().map(|name| (slot, name))
    }) {
        if !selection.allows(move_slot, attacker, target_slot) {
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

// ── The network model ───────────────────────────────────────────────────────

/// Hidden units of [`Mlp`].
///
/// The default network uses one unit per feature.
pub const MLP_HIDDEN: usize = FEATURE_COUNT;

/// Feature scale of the seed network.
///
/// `tanh(x)` is close to `x` for a small `x`, so a diagonal hidden layer at this
/// scale, paired with a scaled output layer, starts the network near its linear
/// input model.
const MLP_SEED_SCALE: f64 = 0.25;

/// One hidden layer with a `tanh` activation and no bias term.
///
/// The score is `logistic(output . tanh(hidden . features))`.
///
/// # Side-swap symmetry
///
/// A mirrored position negates every feature, and `tanh` is an odd function.
/// The hidden vector therefore negates, the output negates, and the logistic map
/// returns one minus the original score.
/// Symmetry holds for every weight matrix, exactly as it does for the linear
/// model, and only because neither layer carries a bias term.
/// Do not add one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mlp<const N: usize, const H: usize> {
    /// One row of feature weights for each hidden unit.
    pub hidden: [[f64; N]; H],
    /// One weight for each hidden unit.
    pub output: [f64; H],
}

impl<const N: usize, const H: usize> Mlp<N, H> {
    /// Builds the network that starts a fit.
    ///
    /// Each feature gets at least one hidden unit.
    /// The output weights divide each linear weight between duplicate units.
    /// The `tanh` function makes this seed close to its linear input model.
    /// Training can fill each off-diagonal entry.
    pub fn seed(weights: &[f64; N]) -> Mlp<N, H> {
        assert!(N > 0, "a network must have at least one feature");
        assert!(H >= N, "a seed network needs at least one unit per feature");
        let mut hidden = [[0.0; N]; H];
        let mut output = [0.0; H];
        for unit in 0..H {
            let feature = unit % N;
            let copies = (H - 1 - feature) / N + 1;
            hidden[unit][feature] = MLP_SEED_SCALE;
            output[unit] = weights[feature] / (MLP_SEED_SCALE * copies as f64);
        }
        Mlp { hidden, output }
    }

    /// The activation of each hidden unit.
    pub fn activations(&self, features: &[f64; N]) -> [f64; H] {
        let mut out = [0.0; H];
        for (activation, row) in out.iter_mut().zip(self.hidden.iter()) {
            let sum: f64 = row
                .iter()
                .zip(features.iter())
                .map(|(weight, value)| weight * value)
                .sum();
            *activation = sum.tanh();
        }
        out
    }

    /// The signed advantage of one position, before the logistic map.
    pub fn advantage(&self, features: &[f64; N]) -> f64 {
        self.activations(features)
            .iter()
            .zip(self.output.iter())
            .map(|(activation, weight)| activation * weight)
            .sum()
    }

    /// P1's predicted win probability.
    pub fn predict(&self, features: &[f64; N]) -> f64 {
        logistic(self.advantage(features))
    }

    /// Whether every weight is a finite number.
    pub fn is_finite(&self) -> bool {
        self.hidden
            .iter()
            .flatten()
            .chain(self.output.iter())
            .all(|value| value.is_finite())
    }
}

/// The value network, as `bin/train_eval` writes it.
///
/// The feature names travel with the matrix, so a feature-order change cannot
/// silently reassign a column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlpRecord {
    /// Feature names, in the column order of `hidden`.
    pub features: Vec<String>,
    /// One row of feature weights for each hidden unit.
    pub hidden: Vec<Vec<f64>>,
    /// One weight for each hidden unit.
    pub output: Vec<f64>,
}

impl MlpRecord {
    /// Builds a record from a network and a name list.
    pub fn from_network<const N: usize, const H: usize>(
        names: &[&str; N],
        network: &Mlp<N, H>,
    ) -> MlpRecord {
        MlpRecord {
            features: names.iter().map(|name| name.to_string()).collect(),
            hidden: network.hidden.iter().map(|row| row.to_vec()).collect(),
            output: network.output.to_vec(),
        }
    }

    /// Reads the record back into a network.
    ///
    /// Returns `None` when a shape disagrees, when a name is missing, or when a
    /// value is not finite. A caller then keeps its fallback evaluator.
    pub fn to_network<const N: usize, const H: usize>(
        &self,
        names: &[&str; N],
    ) -> Option<Mlp<N, H>> {
        if self.hidden.len() != H || self.output.len() != H {
            return None;
        }
        let columns: Vec<usize> = names
            .iter()
            .map(|name| self.features.iter().position(|stored| stored == name))
            .collect::<Option<Vec<usize>>>()?;

        let mut network = Mlp {
            hidden: [[0.0; N]; H],
            output: [0.0; H],
        };
        for (unit, row) in self.hidden.iter().enumerate() {
            if row.len() != self.features.len() {
                return None;
            }
            for (feature, column) in columns.iter().enumerate() {
                network.hidden[unit][feature] = *row.get(*column)?;
            }
            network.output[unit] = self.output[unit];
        }
        network.is_finite().then_some(network)
    }
}

/// The trained value network, as JSON.
const EVAL_MLP_JSON: &str = include_str!("../../weights/eval_mlp_v1.json");

/// Returns the trained value network.
///
/// Parsing runs once. A file that fails to parse returns `None`, because a leaf
/// evaluator must not panic mid-search.
pub fn fitted_network() -> Option<&'static Mlp<FEATURE_COUNT, MLP_HIDDEN>> {
    static CACHE: OnceLock<Option<Mlp<FEATURE_COUNT, MLP_HIDDEN>>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            serde_json::from_str::<MlpRecord>(EVAL_MLP_JSON)
                .ok()
                .and_then(|record| record.to_network(&FEATURE_NAMES))
        })
        .as_ref()
}

/// Scores a position with the trained network.
///
/// `weights/eval_mlp_v1.json` supplies the weights.
/// A file that fails to parse falls back to [`HAND_WEIGHTS`].
pub fn fitted_mlp(state: &BattleState, ctx: &EvalContext<'_>) -> f64 {
    match fitted_network() {
        Some(network) => network.predict(&features(state, ctx)),
        None => score_with(state, ctx, &HAND_WEIGHTS),
    }
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

    /// Builds a Pokemon with a named move set.
    fn mon_with_moves(
        species: Species,
        moves: [Option<PokemonMove>; 4],
        pokemon_dex: &HashMap<Species, PokemonData>,
        move_dex: &HashMap<PokemonMove, MoveData>,
    ) -> PokemonState {
        build_pokemon_state(
            species,
            pokemon_dex,
            move_dex,
            Some(50),
            Some(moves),
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

    /// One named feature of one side, before the subtraction.
    ///
    /// The bench features read one side alone, so a test that reads the
    /// subtracted vector would also move with the other side.
    fn own(state: &BattleState, player: Player, name: &str) -> f64 {
        let ctx = ctx();
        let commands = ActiveCommands::build(state, &ctx);
        side_features(state, player, &ctx, &commands)[feature(name)]
    }

    /// One move on one bench Pokemon, against one opposing active.
    fn bench_position(bench_move: PokemonMove) -> BattleState {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        battle_state_from_lists(
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon_with_moves(
                Species::Pikachu,
                [Some(bench_move), None, None, None],
                pokemon_dex,
                move_dex,
            )],
            vec![mon(Species::Gyarados, pokemon_dex, move_dex)],
            vec![],
        )
    }

    #[test]
    fn a_super_effective_bench_move_raises_bench_threat() {
        // Thunderbolt is four times effective against Water and Flying.
        let weak = own(&bench_position(PokemonMove::Tackle), Player::P1, "bench_threat");
        let strong = own(
            &bench_position(PokemonMove::Thunderbolt),
            Player::P1,
            "bench_threat",
        );
        assert!(weak > 0.0, "a neutral bench move must still threaten");
        assert!(
            strong > weak,
            "bench_threat must rise: {strong} against {weak}"
        );
    }

    /// One move on one opposing active, against one bench Pokemon.
    fn incoming_position(active_move: PokemonMove) -> BattleState {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        battle_state_from_lists(
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Gyarados, pokemon_dex, move_dex)],
            vec![mon_with_moves(
                Species::Pikachu,
                [Some(active_move), None, None, None],
                pokemon_dex,
                move_dex,
            )],
            vec![],
        )
    }

    #[test]
    fn a_super_effective_opposing_move_raises_switch_in_damage() {
        let weak = own(
            &incoming_position(PokemonMove::Tackle),
            Player::P1,
            "switch_in_damage",
        );
        let strong = own(
            &incoming_position(PokemonMove::Thunderbolt),
            Player::P1,
            "switch_in_damage",
        );
        assert!(weak > 0.0, "a neutral opposing move must still hurt");
        assert!(
            strong > weak,
            "switch_in_damage must rise: {strong} against {weak}"
        );
    }

    #[test]
    fn an_empty_bench_reads_the_worst_value_of_both_bench_features() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let state = battle_state_from_lists(
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Gyarados, pokemon_dex, move_dex)],
            vec![],
        );
        assert_eq!(own(&state, Player::P1, "bench_threat"), 0.0);
        assert_eq!(
            own(&state, Player::P1, "switch_in_damage"),
            NO_SWITCH_IN_DAMAGE
        );

        // A fainted bench Pokemon cannot come in, so it reads the same way.
        let mut fainted = bench_position(PokemonMove::Thunderbolt);
        fainted.p1_back_mons[0].hp = 0;
        fainted.p1_back_mons[0].fainted = true;
        assert_eq!(own(&fainted, Player::P1, "bench_threat"), 0.0);
        assert_eq!(
            own(&fainted, Player::P1, "switch_in_damage"),
            NO_SWITCH_IN_DAMAGE
        );
    }

    #[test]
    fn an_empty_bench_reads_one_switch_in_damage_for_each_living_opponent() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![
                mon(Species::Snorlax, pokemon_dex, move_dex),
                mon(Species::Skarmory, pokemon_dex, move_dex),
            ],
            vec![],
            vec![
                mon(Species::Gyarados, pokemon_dex, move_dex),
                mon(Species::Pikachu, pokemon_dex, move_dex),
            ],
            vec![],
        );
        assert_eq!(
            own(&state, Player::P1, "switch_in_damage"),
            2.0 * NO_SWITCH_IN_DAMAGE
        );

        // A fainted opponent cannot punish a switch-in, so it stops counting.
        state.p2_active_mons[1].hp = 0;
        state.p2_active_mons[1].fainted = true;
        assert_eq!(
            own(&state, Player::P1, "switch_in_damage"),
            NO_SWITCH_IN_DAMAGE
        );
    }

    #[test]
    fn an_empty_bench_never_scores_above_an_occupied_one() {
        // `best_attack` returns a fraction from 0 through 1, so an occupied
        // bench cannot reach `NO_SWITCH_IN_DAMAGE` for one opposing active.
        let occupied = own(
            &incoming_position(PokemonMove::Thunderbolt),
            Player::P1,
            "switch_in_damage",
        );
        assert!(
            occupied < NO_SWITCH_IN_DAMAGE,
            "an occupied bench read {occupied}"
        );
    }

    #[test]
    fn the_switch_in_damage_weight_stays_negative() {
        // `NO_SWITCH_IN_DAMAGE` is the worst value of this column only while
        // the weight is negative. A positive weight turns the sentinel into a
        // bonus, and a side with no living bench then scores above a side that
        // can still switch. `bin/train_eval` fits this column, so a run can
        // write a new value into `weights/eval_v1.json`.
        let index = feature("switch_in_damage");
        assert!(
            HAND_WEIGHTS[index] < 0.0,
            "the hand weight read {}",
            HAND_WEIGHTS[index]
        );
        let fitted = fitted_weights()[index];
        assert!(fitted < 0.0, "the committed weight read {fitted}");
    }

    #[test]
    fn covering_the_opposing_team_raises_team_coverage() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let covered = |own_move: PokemonMove| {
            battle_state_from_lists(
                vec![mon_with_moves(
                    Species::Snorlax,
                    [Some(own_move), None, None, None],
                    pokemon_dex,
                    move_dex,
                )],
                vec![],
                vec![mon(Species::Gyarados, pokemon_dex, move_dex)],
                vec![],
            )
        };
        let neutral = own(&covered(PokemonMove::Tackle), Player::P1, "team_coverage");
        let strong = own(
            &covered(PokemonMove::Thunderbolt),
            Player::P1,
            "team_coverage",
        );
        // Neutral is one times, and four times is two doublings.
        assert!((neutral - 0.0).abs() < 1e-9, "neutral read {neutral}");
        assert!((strong - 2.0).abs() < 1e-9, "four times read {strong}");
    }

    /// A slot index is a label, so exchanging the two active slots of both sides
    /// must not move a feature.
    ///
    /// The bench features read a slot that no Pokemon owns. A constant index
    /// there reads the ally of slot zero, which an exchange moves.
    #[test]
    fn exchanging_the_active_slots_does_not_move_the_bench_features() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        // The switch-in holds Minus and one special move, so a Plus ally raises
        // `bench_threat`. A Friend Guard ally lowers `switch_in_damage`. Both
        // readings come from the ally of the entry slot.
        let position = |ally: Ability| {
            let mut state = battle_state_from_lists(
                vec![
                    mon(Species::Pikachu, pokemon_dex, move_dex),
                    mon(Species::Clefable, pokemon_dex, move_dex),
                ],
                vec![mon_with_moves(
                    Species::Snorlax,
                    [Some(PokemonMove::Thunderbolt), None, None, None],
                    pokemon_dex,
                    move_dex,
                )],
                vec![
                    mon(Species::Gyarados, pokemon_dex, move_dex),
                    mon(Species::Gengar, pokemon_dex, move_dex),
                ],
                vec![],
            );
            state.p1_active_mons[1].ability = ally;
            state.p1_back_mons[0].ability = Ability::Minus;
            state
        };
        let exchanged = |state: &BattleState| {
            let mut out = state.clone();
            out.p1_active_mons.swap(0, 1);
            out.p2_active_mons.swap(0, 1);
            out
        };

        // The ally must reach both readings, or the invariance below holds for
        // no reason.
        let plain = position(Ability::CuteCharm);
        assert!(
            own(&position(Ability::Plus), Player::P1, "bench_threat")
                > own(&plain, Player::P1, "bench_threat"),
            "a Plus ally did not reach bench_threat"
        );
        assert!(
            own(&position(Ability::FriendGuard), Player::P1, "switch_in_damage")
                < own(&plain, Player::P1, "switch_in_damage"),
            "a Friend Guard ally did not reach switch_in_damage"
        );

        for ally in [Ability::CuteCharm, Ability::Plus, Ability::FriendGuard] {
            let state = position(ally.clone());
            let swapped = exchanged(&state);
            for name in ["bench_threat", "switch_in_damage", "team_coverage"] {
                let before = own(&state, Player::P1, name);
                let after = own(&swapped, Player::P1, name);
                assert!(
                    (before - after).abs() < 1e-12,
                    "{name} moved from {before} to {after} with a {ally:?} ally"
                );
            }
            let before = heuristic(&state, &ctx());
            let after = heuristic(&swapped, &ctx());
            assert!(
                (before - after).abs() < 1e-12,
                "the score moved from {before} to {after} with a {ally:?} ally"
            );
        }
    }

    /// The type-chart proxy must read the move type that the damage calculation
    /// reads.
    #[test]
    fn a_type_changing_ability_reaches_the_type_edge() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let position = |ability: Ability| {
            let mut state = battle_state_from_lists(
                vec![mon_with_moves(
                    Species::Sylveon,
                    [Some(PokemonMove::HyperVoice), None, None, None],
                    pokemon_dex,
                    move_dex,
                )],
                vec![],
                vec![mon(Species::Dragonite, pokemon_dex, move_dex)],
                vec![],
            );
            state.p1_active_mons[0].ability = ability;
            state
        };
        // Hyper Voice is Normal, which Dragon and Flying both take neutrally.
        let plain = own(&position(Ability::CuteCharm), Player::P1, "team_coverage");
        // Pixilate makes it Fairy, and Dragon takes Fairy for double damage.
        let converted = own(&position(Ability::Pixilate), Player::P1, "team_coverage");
        assert!((plain - 0.0).abs() < 1e-9, "Normal read {plain}");
        assert!((converted - 1.0).abs() < 1e-9, "Fairy read {converted}");
    }

    #[test]
    fn a_mirrored_doubles_position_with_a_bench_is_even() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let side = || {
            vec![
                mon(Species::Pikachu, pokemon_dex, move_dex),
                mon(Species::Gyarados, pokemon_dex, move_dex),
            ]
        };
        let bench = || {
            vec![
                mon(Species::Snorlax, pokemon_dex, move_dex),
                mon(Species::Skarmory, pokemon_dex, move_dex),
            ]
        };
        let state = battle_state_from_lists(side(), bench(), side(), bench());
        let values = features(&state, &ctx());
        for name in ["bench_threat", "switch_in_damage", "team_coverage"] {
            assert!(
                values[feature(name)].abs() < 1e-12,
                "{name} read {}",
                values[feature(name)]
            );
        }
        assert!((heuristic(&state, &ctx()) - 0.5).abs() < 1e-9);
    }

    /// The name-to-index map that the field tests read.
    fn feature(name: &str) -> usize {
        FEATURE_NAMES
            .iter()
            .position(|entry| *entry == name)
            .unwrap_or_else(|| panic!("no feature named {name}"))
    }

    /// Weather is field state, so a mirrored position under weather must still
    /// score even. This is the property that stops a raw weather indicator from
    /// entering the frame.
    #[test]
    fn weather_alone_keeps_a_mirrored_position_even() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
        );
        for weather in [
            Weather::Rain,
            Weather::Sun,
            Weather::Sandstorm,
            Weather::Snow,
        ] {
            state.weather = Some(weather.clone());
            state.weather_turns = Some(5);
            let values = features(&state, &ctx());
            assert!(
                values[feature("weather_edge")].abs() < 1e-12,
                "{weather:?} left a one-sided edge"
            );
            assert!((heuristic(&state, &ctx()) - 0.5).abs() < 1e-9, "{weather:?}");
        }
    }

    /// The same weather charged against different Pokemon must move the feature.
    /// Rain helps a Water-type and hurts a Fire-type.
    #[test]
    fn rain_favors_the_water_side_over_the_fire_side() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Squirtle, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Charmander, pokemon_dex, move_dex)],
            vec![],
        );
        let dry = features(&state, &ctx())[feature("weather_edge")];
        assert!(dry.abs() < 1e-12, "no weather must read zero, got {dry}");

        state.weather = Some(Weather::Rain);
        state.weather_turns = Some(5);
        let wet = features(&state, &ctx())[feature("weather_edge")];
        assert!(wet > 0.0, "rain must favor the Water side, got {wet}");

        state.weather = Some(Weather::Sun);
        let sunny = features(&state, &ctx())[feature("weather_edge")];
        assert!(sunny < 0.0, "sun must favor the Fire side, got {sunny}");
    }

    /// A shorter timer is worth less than a fresh one.
    #[test]
    fn a_spent_weather_timer_lowers_the_edge() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Squirtle, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Charmander, pokemon_dex, move_dex)],
            vec![],
        );
        state.weather = Some(Weather::Rain);
        state.weather_turns = Some(5);
        let fresh = features(&state, &ctx())[feature("weather_edge")];
        state.weather_turns = Some(1);
        let spent = features(&state, &ctx())[feature("weather_edge")];
        assert!(spent < fresh, "spent {spent} must be under fresh {fresh}");
        assert!(spent > 0.0, "a live timer must keep the sign");
    }

    /// Tailwind is stored per side, so it needs no re-expression. It must be
    /// charged to the side that owns it.
    #[test]
    fn tailwind_is_charged_to_its_own_side() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
        );
        let even = heuristic(&state, &ctx());
        assert!((even - 0.5).abs() < 1e-9);

        state.p1_side_conditions.push(SideCondition::TailWind);
        state.p1_side_condition_turns.push(4);
        let values = features(&state, &ctx());
        assert!(values[feature("tailwind")] > 0.0);
        assert!(heuristic(&state, &ctx()) > even);

        state.p2_side_conditions.push(SideCondition::TailWind);
        state.p2_side_condition_turns.push(4);
        assert!((heuristic(&state, &ctx()) - 0.5).abs() < 1e-9, "both sides");
    }

    /// Safeguard, Mist, and Lucky Chant are status guards, and none of them may
    /// leak into the damage-reducing screen count.
    #[test]
    fn status_guards_stay_out_of_the_screen_feature() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
        );
        state.p1_side_conditions.push(SideCondition::SafeGuard);
        state.p1_side_condition_turns.push(5);
        let values = features(&state, &ctx());
        assert!(values[feature("guard_conditions")] > 0.0);
        assert!(values[feature("screens")].abs() < 1e-12);
    }

    /// Trick Room reverses the order, which `speed` already reads. This feature
    /// prices the remaining clock, so it must fall as the clock runs out and it
    /// must read zero when Trick Room is not up.
    #[test]
    fn the_trick_room_feature_prices_the_clock() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
        );
        let index = feature("trick_room");
        assert!(features(&state, &ctx())[index].abs() < 1e-12, "no room");

        state.pseudo_weathers.push(PseudoWeather::TrickRoom);
        state.pseudo_weather_turns.push(5);
        let fresh = features(&state, &ctx())[index];
        assert!(fresh > 0.0, "the slow side must gain, got {fresh}");

        state.pseudo_weather_turns[0] = 1;
        let spent = features(&state, &ctx())[index];
        assert!(spent < fresh, "spent {spent} must be under fresh {fresh}");
    }

    /// Every new feature must read zero on a position that has no field state.
    /// A feature that is nonzero on an empty field is measuring something else.
    #[test]
    fn the_field_features_read_zero_without_field_state() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![mon(Species::Snorlax, pokemon_dex, move_dex)],
            vec![mon(Species::Charmander, pokemon_dex, move_dex)],
            vec![mon(Species::Squirtle, pokemon_dex, move_dex)],
        );
        let values = features(&state, &ctx());
        for name in [
            "weather_edge",
            "terrain_edge",
            "tailwind",
            "guard_conditions",
            "trick_room",
        ] {
            assert!(
                values[feature(name)].abs() < 1e-12,
                "{name} read {} on an empty field",
                values[feature(name)]
            );
        }
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

    /// Indexes of `guaranteed_kill` and `possible_kill` in [`FEATURE_NAMES`].
    const GUARANTEED: usize = 6;
    const POSSIBLE: usize = 7;

    #[test]
    fn a_certain_kill_reads_both_kill_features_at_one() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let mut state = battle_state_from_lists(
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
            vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
            vec![],
        );
        // Tackle never misses, and one point of HP dies to every roll.
        state.p2_active_mons[0].hp = 1;
        let values = features(&state, &ctx());
        assert_eq!(values[GUARANTEED], 1.0);
        assert!((values[POSSIBLE] - 1.0).abs() < 1e-9);
    }

    /// Sixteen rolls must separate a certain kill from a likely one.
    ///
    /// One roll made the kill mass zero or one, so `possible_kill` equalled
    /// `guaranteed_kill` on every move that cannot miss. The two features were
    /// then collinear, and no corpus could weight them apart.
    #[test]
    fn a_partial_kill_reads_possible_kill_between_zero_and_one() {
        let pokemon_dex = pokemon_dex();
        let move_dex = move_dex();
        let build = |target_hp: u16| {
            let mut state = battle_state_from_lists(
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![],
                vec![mon(Species::Pikachu, pokemon_dex, move_dex)],
                vec![],
            );
            state.p2_active_mons[0].hp = target_hp;
            features(&state, &ctx())
        };

        // Damage numbers change with the dex, so the test searches for the HP
        // band where the rolls disagree instead of naming one.
        let partial = (1..=60u16)
            .map(build)
            .find(|values| values[POSSIBLE] > 0.0 && values[POSSIBLE] < 1.0);
        let values = partial.expect("some HP total must split the damage rolls");
        assert_eq!(
            values[GUARANTEED], 0.0,
            "a roll that fails to kill must clear the guaranteed flag"
        );
    }

    #[test]
    fn the_network_returns_one_minus_its_own_score_for_a_mirrored_position() {
        let network = Mlp::<FEATURE_COUNT, MLP_HIDDEN>::seed(&HAND_WEIGHTS);
        let mut values = [0.0; FEATURE_COUNT];
        for (index, slot) in values.iter_mut().enumerate() {
            *slot = 0.3 * (index as f64) - 1.1;
        }
        let mirrored: Features = std::array::from_fn(|index| -values[index]);
        let score = network.predict(&values);
        assert!((network.predict(&mirrored) + score - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_shipped_network_round_trips_through_its_record() {
        let network = fitted_network().expect("the shipped network must parse");
        let record = MlpRecord::from_network(&FEATURE_NAMES, network);
        let restored: Mlp<FEATURE_COUNT, MLP_HIDDEN> = record
            .to_network(&FEATURE_NAMES)
            .expect("a written record must read back");
        assert_eq!(&restored, network);
    }

    #[test]
    fn a_damaged_network_file_falls_back_to_the_hand_weights() {
        for text in [
            "not json",
            r#"{"features":["health"],"hidden":[[1.0]],"output":[1.0]}"#,
            r#"{"features":[],"hidden":[],"output":[]}"#,
        ] {
            let restored = serde_json::from_str::<MlpRecord>(text)
                .ok()
                .and_then(|record| record.to_network::<FEATURE_COUNT, MLP_HIDDEN>(&FEATURE_NAMES));
            assert!(restored.is_none(), "accepted a damaged record: {text}");
        }
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
