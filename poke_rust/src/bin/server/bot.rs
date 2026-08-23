//! One solver profile of a session.
//!
//! A create-battle request can carry one profile, and a tracker analysis
//! request carries the same shape. The profile names one search algorithm and
//! raw search limits.
//!
//! `resolve` validates the request and returns the resolved profile. The profile
//! holds a wire view for the client and one concrete solver configuration for
//! the search. The wire types live here, so no Serde derive reaches engine
//! state.
//!
//! Each caller passes its own `scope`, which names the request field in every
//! error line. The battle endpoint sends `botP2`, and the tracker analysis
//! endpoint sends `analysis`.
//!
//! The profile does not start a search itself. `analysis.rs` owns the battle
//! job, and `tracker_analysis.rs` owns the tracker job. Both read
//! `BotProfile::search`.

use serde::{Deserialize, Serialize};

use poke_rust::solver::ismcts::IsmctsConfig;
use poke_rust::solver::mccfr::MccfrConfig;
use poke_rust::solver::mcts::MctsConfig;
use poke_rust::solver::pimc::PimcConfig;
use poke_rust::solver::{SolveConfig, SolverAlgorithm};

/// The search that a P2 bot profile configures.
///
/// The first three names solve the game exactly to the depth horizon. The last
/// four names sample, so they return an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotAlgorithm {
    BackwardInduction,
    SerializedBounds,
    DoubleOracle,
    Mcts,
    Ismcts,
    Mccfr,
    Pimc,
}

impl BotAlgorithm {
    pub(crate) fn from_wire(scope: &str, name: &str) -> Result<Self, String> {
        match name {
            "backwardInduction" => Ok(BotAlgorithm::BackwardInduction),
            "serializedBounds" => Ok(BotAlgorithm::SerializedBounds),
            "doubleOracle" => Ok(BotAlgorithm::DoubleOracle),
            "mcts" => Ok(BotAlgorithm::Mcts),
            "ismcts" => Ok(BotAlgorithm::Ismcts),
            "mccfr" => Ok(BotAlgorithm::Mccfr),
            "pimc" => Ok(BotAlgorithm::Pimc),
            other => Err(format!("{scope}.algorithm: unknown algorithm {other:?}")),
        }
    }

    fn wire_name(self) -> &'static str {
        match self {
            BotAlgorithm::BackwardInduction => "backwardInduction",
            BotAlgorithm::SerializedBounds => "serializedBounds",
            BotAlgorithm::DoubleOracle => "doubleOracle",
            BotAlgorithm::Mcts => "mcts",
            BotAlgorithm::Ismcts => "ismcts",
            BotAlgorithm::Mccfr => "mccfr",
            BotAlgorithm::Pimc => "pimc",
        }
    }

    /// True when the search evaluates every action to the depth horizon.
    pub fn is_exact(self) -> bool {
        matches!(
            self,
            BotAlgorithm::BackwardInduction
                | BotAlgorithm::SerializedBounds
                | BotAlgorithm::DoubleOracle
        )
    }

    /// True when the search draws worlds from a belief.
    /// Only these three algorithms read the `particles` limit.
    fn uses_particles(self) -> bool {
        matches!(
            self,
            BotAlgorithm::Ismcts | BotAlgorithm::Mccfr | BotAlgorithm::Pimc
        )
    }

    /// True when the profile may reach its depth by refinement.
    ///
    /// A refinement pass solves a matrix, so it needs an algorithm that solves a
    /// matrix. A sampled search walks trajectories instead and has no support to
    /// raise. `Pimc` qualifies, because each of its worlds is an exact search.
    fn supports_refinement(self) -> bool {
        self.is_exact() || self == BotAlgorithm::Pimc
    }

    fn exact_algorithm(self) -> SolverAlgorithm {
        match self {
            BotAlgorithm::SerializedBounds => SolverAlgorithm::SerializedBounds,
            BotAlgorithm::BackwardInduction => SolverAlgorithm::BackwardInduction,
            _ => SolverAlgorithm::DoubleOracle,
        }
    }
}

/// The largest integer that JavaScript can represent without precision loss.
/// `analysis::random_seed` draws inside this range for the same reason.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The limits that one preset name resolves to.
///
/// A request field replaces the matching value. An absent field keeps it.
#[derive(Debug, Clone, Copy)]
pub struct PresetLimits {
    /// The budget of a search that finishes by itself.
    ///
    /// An exact search and `pimc` stop when the matrix is solved. The budget
    /// must hold that solve. A budget below it returns a matrix of static
    /// scores, and a matrix of static scores names no action.
    pub simulation_turn_budget: u64,
    /// The budget of a search that never finishes by itself.
    ///
    /// `mcts`, `ismcts`, and `mccfr` spend every turn they are given, so the
    /// budget decides how many seconds one answer takes rather than whether the
    /// answer is complete. It is therefore a much smaller number than
    /// `simulation_turn_budget`.
    pub sampled_simulation_turn_budget: u64,
    pub depth: u8,
    pub damage_rolls: u8,
    pub particles: usize,
}

impl PresetLimits {
    /// The budget that one algorithm takes from this preset.
    pub fn budget_for_algorithm(&self, algorithm: BotAlgorithm) -> u64 {
        if algorithm.is_exact() || algorithm == BotAlgorithm::Pimc {
            self.simulation_turn_budget
        } else {
            self.sampled_simulation_turn_budget
        }
    }
}

/// The depth that every preset runs.
///
/// A depth of one resolves the turn and scores each outcome with the leaf
/// evaluator. Depth two costs a whole depth-one solve for each cell of the root
/// matrix, and `benches/RESULTS.md` records that a doubles position needs about
/// eight minutes for one round of it. The presets therefore spend their budget
/// on the width of one turn instead of on a second turn: more damage rolls give
/// the leaf evaluator a truer outcome distribution, and more worlds give a
/// belief search a truer opponent.
///
/// A request may still ask for a deeper search. `SolverControls` offers the
/// field, and `refine` reaches a deeper cell without a complete deep pass.
pub const PRESET_DEPTH: u8 = 1;

/// The depth that a refined profile solves completely before it raises cells.
///
/// One turn of lookahead over the complete action set is the answer that every
/// later round improves. A doubles position finishes it in about one third of a
/// second at one damage roll.
///
/// Every preset now runs at [`PRESET_DEPTH`], which is this same depth, so a
/// refined preset has nothing to raise and returns the base answer. Refinement
/// applies when a request raises the depth itself.
pub const REFINE_BASE_DEPTH: u8 = 1;

/// The largest particle count that a request may ask for.
///
/// Each world of `pimc` is a complete solve, so this count multiplies the cost
/// of that search directly. `ismcts` and `mccfr` draw the same number of worlds
/// and share one budget across them, so the count changes the spread of their
/// sampling rather than their cost.
pub const MAX_PARTICLES: usize = 64;

/// The budget of one preset, from the damage rolls that the preset reads.
///
/// `cargo bench --bench depth1_budget` measured one depth-one solve of the
/// cheapest doubles pairing, at 290 actions against 370.
///
/// | Damage rolls | Turn simulations | Time on 22 workers |
/// |---|---|---|
/// | 3 | 104,576 | 5.9s |
/// | 8 | 872,971 | 123s |
/// | 16 | 1,339,655 | 216s |
///
/// The budget must hold two of those solves. `pimc::GUARANTEED_WORLDS` is two,
/// and two searched worlds say more than a full particle set of unsearched
/// ones. The extra factor of two covers a pairing more costly than this one.
///
/// Singles never reaches these numbers. The same sweep measured 432 turn
/// simulations for a singles position at sixteen rolls, so the budget binds on
/// doubles alone.
const fn budget_for(measured_doubles_solve: u64) -> u64 {
    measured_doubles_solve * 2 * 2
}

/// The budget of a sampled search, from the seconds that one answer may take.
///
/// A sampled search spends every turn that it gets, so no budget makes it
/// complete. The same sweep measured 6,607 turn simulations for each second on
/// one thread, at one damage roll, on the same doubles position.
///
/// A damage roll lowers that rate, because one turn simulation then builds more
/// branches. The exact sweep measured the same slowdown, and these are the
/// factors it reports against one roll.
///
/// | Damage rolls | Slower by |
/// |---|---|
/// | 3 | 2.4 |
/// | 8 | 5.9 |
/// | 16 | 6.9 |
const fn sampled_budget_for(turns_for_each_second: u64, seconds: u64) -> u64 {
    turns_for_each_second * seconds
}

/// Resolves the fastest answer. It is the preset that a live battle can wait on.
///
/// Three rolls read the low, middle, and high damage of every attack. That is
/// the smallest set that separates a roll which faints a target from a roll
/// which does not.
pub const FAST_PRESET: PresetLimits = PresetLimits {
    simulation_turn_budget: budget_for(104_576),
    sampled_simulation_turn_budget: sampled_budget_for(2_753, 10),
    depth: PRESET_DEPTH,
    damage_rolls: 3,
    particles: 12,
};

/// The default preset. It reads every other damage roll.
///
/// One doubles answer takes minutes here. Use `fast` for a live doubles battle.
pub const BALANCED_PRESET: PresetLimits = PresetLimits {
    simulation_turn_budget: budget_for(872_971),
    sampled_simulation_turn_budget: sampled_budget_for(1_120, 30),
    depth: PRESET_DEPTH,
    damage_rolls: 8,
    particles: 24,
};

/// The widest preset. It reads every damage roll, so no outcome of an attack
/// stays out of the leaf distribution.
///
/// The measured value moved by 0.002 between two rolls and sixteen, and the
/// equilibrium support held five actions at every roll count. Read
/// `benches/RESULTS.md` before you choose this preset for accuracy.
pub const COMPETITIVE_PRESET: PresetLimits = PresetLimits {
    simulation_turn_budget: budget_for(1_339_655),
    sampled_simulation_turn_budget: sampled_budget_for(958, 90),
    depth: PRESET_DEPTH,
    damage_rolls: 16,
    particles: 48,
};

/// The limits that a request without a preset name takes.
pub const DEFAULT_PRESET: PresetLimits = BALANCED_PRESET;

// No preset may ask for more worlds than a request may. `check_range` rejects a
// request above the cap, and a preset never reaches that check.
const _: () = assert!(FAST_PRESET.particles <= MAX_PARTICLES);
const _: () = assert!(BALANCED_PRESET.particles <= MAX_PARTICLES);
const _: () = assert!(COMPETITIVE_PRESET.particles <= MAX_PARTICLES);

/// The profile as the client sends it.
/// Every field is optional. The server supplies speed-first defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BotProfileRequest {
    /// One of `fast`, `balanced`, `competitive`, or `custom`.
    pub preset: Option<String>,
    /// Defaults to `doubleOracle`.
    ///
    /// `routes::resolve_bot_p2` fills an absent field before this call, because
    /// `doubleOracle` cannot control P2 in a fog-of-war session. A battle
    /// request therefore takes the search of its information mode.
    pub algorithm: Option<String>,
    /// The turn simulations that the whole search may spend.
    ///
    /// An absent field takes the fixed budget of the selected preset.
    pub simulation_turn_budget: Option<u64>,
    pub depth: Option<u8>,
    pub damage_rolls: Option<u8>,
    pub consider_crit: Option<bool>,
    /// Turns of lookahead below a replacement or a self-switch pivot.
    /// An absent field gives a forced decision the remaining turn depth.
    pub replacement_depth: Option<u8>,
    /// Reaches `depth` by refinement instead of by a complete search.
    ///
    /// Applies to an exact algorithm and to `pimc`, whose worlds are exact.
    pub refine: Option<bool>,
    /// Belief searches only.
    pub particles: Option<usize>,
    /// Makes a sampled search reproducible.
    /// The maximum is JavaScript's largest safe integer.
    pub seed: Option<u64>,
    /// Kept for request compatibility. Bot sessions always show the strategy.
    #[allow(dead_code)]
    pub reveal_strategy: Option<bool>,
}

/// The resolved profile as the client reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BotProfileView {
    pub algorithm: String,
    /// True when the algorithm itself is exact.
    /// A limit can still make the result approximate.
    pub exact: bool,
    /// The resolved budget, whether the client sent it or the server derived it.
    pub simulation_turn_budget: u64,
    pub depth: u8,
    pub damage_rolls: u8,
    pub consider_crit: bool,
    /// Absent when a forced decision uses the remaining turn budget.
    pub replacement_depth: Option<u8>,
    /// True when the search reaches its depth by refinement.
    pub refine: bool,
    /// The workers that this search asks the process pool for.
    ///
    /// A busy pool can give the search fewer workers. The count does not change
    /// the value or either strategy.
    pub workers: u8,
    pub particles: Option<usize>,
    pub seed: Option<u64>,
    /// True when the client may read Player 2's strategy.
    ///
    /// The two battle endpoints render a strategy row only for a true value.
    /// See [`BotProfileRequest::reveal_strategy`].
    pub reveal_strategy: bool,
    /// Each reason that the result can differ from the exact answer.
    /// The interface shows this list.
    pub approximations: Vec<String>,
}

/// The concrete solver configuration of one profile.
///
/// `analysis::run_search` routes each variant to its solver entry point.
#[derive(Debug, Clone, Copy)]
pub enum BotSearchConfig {
    Exact(SolveConfig),
    /// A complete search at `base_depth`, then refinement up to `solve.depth`.
    Refine {
        solve: SolveConfig,
        base_depth: u8,
    },
    Mcts(MctsConfig),
    Ismcts(IsmctsConfig),
    Mccfr(MccfrConfig),
    Pimc(PimcConfig),
}

impl BotSearchConfig {
    /// The first depth that a server search runs.
    ///
    /// An exact search can finish a rung. A sampled search runs at the requested
    /// depth until it uses the shared simulation-turn budget.
    ///
    /// `Pimc` runs one rung, although each world of it is an exact solve. A
    /// ladder would multiply the world count by the depth. That search publishes
    /// an answer after each world instead, so the panel still moves.
    pub fn first_depth(self, requested: u8) -> u8 {
        match self {
            BotSearchConfig::Exact(_) => 1,
            // A refinement pass runs its own ladder inside one call: it solves
            // the base depth completely and then raises cells. A rung above that
            // would repeat the base pass.
            BotSearchConfig::Refine { .. } => requested,
            _ => requested,
        }
    }

    /// True when the search draws its worlds from a belief.
    ///
    /// Such a search never reads the true position. It can therefore control
    /// Player 2 in a fog-of-war session, and the tracker panel accepts it. Every
    /// other search needs a concrete `MatchState`.
    ///
    /// The rows of the other player mix the private builds of several worlds, so
    /// a belief search also reports a strategy that the other player could not
    /// play. `tracker_analysis::p2_strategy_is_playable` holds that second rule.
    pub fn searches_belief(self) -> bool {
        matches!(
            self,
            BotSearchConfig::Ismcts(_) | BotSearchConfig::Mccfr(_) | BotSearchConfig::Pimc(_)
        )
    }

    /// The same configuration at another depth horizon.
    ///
    /// The tracker ladder in `tracker_analysis.rs` runs one rung for each depth
    /// from one through the configured depth, so it needs this copy.
    pub fn with_depth(self, depth: u8) -> BotSearchConfig {
        match self {
            BotSearchConfig::Exact(config) => {
                BotSearchConfig::Exact(SolveConfig { depth, ..config })
            }
            BotSearchConfig::Refine { solve, base_depth } => BotSearchConfig::Refine {
                solve: SolveConfig { depth, ..solve },
                // The base pass must stay at or below the refined depth. A rung
                // at the base depth then has nothing to raise and returns the
                // base answer, which is the honest result for that rung.
                base_depth: base_depth.min(depth),
            },
            BotSearchConfig::Mcts(config) => BotSearchConfig::Mcts(MctsConfig { depth, ..config }),
            BotSearchConfig::Ismcts(config) => BotSearchConfig::Ismcts(IsmctsConfig {
                search: MctsConfig {
                    depth,
                    ..config.search
                },
                ..config
            }),
            BotSearchConfig::Mccfr(config) => BotSearchConfig::Mccfr(MccfrConfig {
                search: MctsConfig {
                    depth,
                    ..config.search
                },
                ..config
            }),
            BotSearchConfig::Pimc(config) => BotSearchConfig::Pimc(PimcConfig {
                solve: SolveConfig {
                    depth,
                    ..config.solve
                },
                ..config
            }),
        }
    }
}

/// The stored profile of one session.
#[derive(Debug, Clone)]
pub struct BotProfile {
    /// The view that both battle endpoints return.
    pub view: BotProfileView,
    /// The configuration that the analysis job passes to the solver.
    pub search: BotSearchConfig,
}

fn preset_limits(scope: &str, preset: Option<&str>) -> Result<PresetLimits, String> {
    match preset.unwrap_or("balanced") {
        "fast" => Ok(FAST_PRESET),
        "balanced" => Ok(BALANCED_PRESET),
        "competitive" => Ok(COMPETITIVE_PRESET),
        "custom" => Ok(DEFAULT_PRESET),
        name => Err(format!("{scope}.preset: unknown preset {name:?}")),
    }
}

/// Rejects a value outside its permitted range.
fn check_range<T: PartialOrd + std::fmt::Display>(
    scope: &str,
    field: &str,
    value: T,
    low: T,
    high: T,
) -> Result<(), String> {
    if value < low || value > high {
        return Err(format!(
            "{scope}.{field}: {value} is outside the range {low} through {high}"
        ));
    }
    Ok(())
}

/// Rejects a field that the selected algorithm does not read.
fn reject_unused(scope: &str, present: bool, field: &str, reason: &str) -> Result<(), String> {
    if present {
        return Err(format!("{scope}.{field}: {reason}"));
    }
    Ok(())
}

/// Validates one request and resolves its raw search limits.
///
/// `scope` names the request field in every error line. The battle endpoint
/// passes `botP2`, and the tracker analysis endpoint passes `analysis`.
///
pub fn resolve(scope: &str, req: &BotProfileRequest) -> Result<BotProfile, String> {
    let preset = preset_limits(scope, req.preset.as_deref())?;
    let algorithm = match &req.algorithm {
        Some(name) => BotAlgorithm::from_wire(scope, name)?,
        None => BotAlgorithm::DoubleOracle,
    };
    // Reject a limit that the selected algorithm cannot read.
    reject_unused(
        scope,
        req.particles.is_some() && !algorithm.uses_particles(),
        "particles",
        "a particle count applies only to ismcts, mccfr, or pimc",
    )?;
    reject_unused(
        scope,
        req.seed.is_some() && algorithm.is_exact(),
        "seed",
        "a seed applies only to a sampling algorithm",
    )?;
    reject_unused(
        scope,
        req.refine.is_some() && !algorithm.supports_refinement(),
        "refine",
        "refinement applies only to an exact algorithm or to pimc",
    )?;

    let depth = req.depth.unwrap_or(preset.depth);
    check_range(scope, "depth", depth, 1, 8)?;
    let replacement_depth = match req.replacement_depth {
        Some(value) => {
            check_range(scope, "replacementDepth", value, 1, 8)?;
            Some(value)
        }
        None => None,
    };
    let damage_rolls = req.damage_rolls.unwrap_or(preset.damage_rolls);
    check_range(scope, "damageRolls", damage_rolls, 1, 16)?;
    let consider_crit = req.consider_crit.unwrap_or(false);
    let particles = match req.particles {
        Some(value) => {
            check_range(scope, "particles", value, 1, MAX_PARTICLES)?;
            value
        }
        None => preset.particles,
    };
    let exact = algorithm.is_exact();
    let refine = req.refine.unwrap_or(false) && algorithm.supports_refinement();
    if let Some(seed) = req.seed {
        check_range(scope, "seed", seed, 0, MAX_SAFE_INTEGER)?;
    }
    let particles = algorithm.uses_particles().then_some(particles);
    // An exact search and `pimc` finish, so their budget must hold one solve. A
    // sampled search spends every turn it gets, so its budget sets the seconds
    // of one answer. `PresetLimits` holds one number for each case.
    let simulation_turn_budget = req
        .simulation_turn_budget
        .unwrap_or_else(|| preset.budget_for_algorithm(algorithm));
    check_range(
        scope,
        "simulationTurnBudget",
        simulation_turn_budget,
        1,
        1_000_000_000,
    )?;

    let mut approximations = Vec::new();
    approximations.push(format!(
        "The search stops after {depth} turn(s) and scores the position with the leaf evaluator."
    ));
    if let Some(value) = replacement_depth {
        approximations.push(format!(
            "A forced switch is searched to {value} turn(s) instead of the remaining budget. One path can pass that budget one time."
        ));
    }
    if !exact && algorithm != BotAlgorithm::Pimc {
        approximations
            .push("The search samples trajectories, so the strategy is an estimate.".to_string());
    }
    if let Some(count) = particles {
        approximations.push(format!(
            "The search draws {count} world(s) from the belief, so hidden data is sampled."
        ));
    }
    if algorithm == BotAlgorithm::Pimc {
        approximations.push(
            "The search solves each world separately and averages the strategies. Each world \
             therefore plays as if the hidden data were known (strategy fusion), so the answer \
             claims more than a real player can do."
                .to_string(),
        );
    }
    if refine {
        approximations.push(format!(
            "The search solves depth {REFINE_BASE_DEPTH} over every action, then raises only the \
             cells that decide the answer to depth {depth}. An action that it never raises was \
             ranked at depth {REFINE_BASE_DEPTH} alone, and the answer reports how many."
        ));
    }
    approximations.push(format!(
        "The search can simulate at most {simulation_turn_budget} turn(s). It uses static scores after the budget is exhausted."
    ));
    if damage_rolls < 16 {
        approximations.push(format!(
            "Each attack uses {damage_rolls} representative damage roll(s) instead of all 16 rolls."
        ));
    }
    if !consider_crit {
        approximations.push("The search does not include critical-hit branches.".to_string());
    }
    let search = build_search(
        algorithm,
        depth,
        replacement_depth,
        particles,
        damage_rolls,
        consider_crit,
        refine,
    );

    Ok(BotProfile {
        view: BotProfileView {
            algorithm: algorithm.wire_name().to_string(),
            exact,
            simulation_turn_budget,
            depth,
            damage_rolls,
            consider_crit,
            replacement_depth,
            refine,
            workers: search_workers(&search),
            particles,
            seed: req.seed,
            reveal_strategy: true,
            approximations,
        },
        search,
    })
}

/// The workers that one resolved search uses.
///
/// Only double oracle uses the worker pool. Every other search runs on one
/// thread.
fn search_workers(search: &BotSearchConfig) -> u8 {
    let workers = match search {
        BotSearchConfig::Exact(config) => config,
        // Each world of a PIMC search is a double-oracle solve, so the root of
        // that world batches its cells over the same pool.
        BotSearchConfig::Pimc(config) => &config.solve,
        _ => return 1,
    };
    if workers.algorithm != SolverAlgorithm::DoubleOracle {
        return 1;
    }
    workers.workers.min(u8::MAX as usize) as u8
}

/// Builds the solver configuration of one resolved profile.
#[allow(clippy::too_many_arguments)]
fn build_search(
    algorithm: BotAlgorithm,
    depth: u8,
    replacement_depth: Option<u8>,
    particles: Option<usize>,
    damage_rolls: u8,
    consider_crit: bool,
    refine: bool,
) -> BotSearchConfig {
    let exact = SolveConfig {
        depth,
        replacement_depth,
        iterative_deepening: true,
        damage_rolls,
        consider_crit,
        algorithm: algorithm.exact_algorithm(),
        max_actions_per_player: None,
        node_budget: None,
        deadline: None,
        // The pool bounds the extra threads across every concurrent solve,
        // so the server can ask for the whole cap here.
        workers: poke_rust::solver::pool::shared().capacity(),
        ..SolveConfig::default()
    };
    if algorithm.is_exact() {
        // A refined profile runs its own base pass, so it takes no outer ladder.
        return if refine {
            BotSearchConfig::Refine {
                solve: SolveConfig {
                    iterative_deepening: false,
                    ..exact
                },
                base_depth: REFINE_BASE_DEPTH.min(depth),
            }
        } else {
            BotSearchConfig::Exact(exact)
        };
    }
    if algorithm == BotAlgorithm::Pimc {
        // Each world is a complete perfect-information solve, so it takes the
        // configuration that an exact profile builds. `exact_algorithm` gives
        // double oracle to every name that is not exact itself.
        let base = PimcConfig::default();
        return BotSearchConfig::Pimc(PimcConfig {
            solve: SolveConfig {
                iterative_deepening: !refine,
                ..exact
            },
            refine_base_depth: refine.then(|| REFINE_BASE_DEPTH.min(depth)),
            particles: particles.unwrap_or(base.particles),
            ..base
        });
    }

    let search = MctsConfig {
        // The shared simulation-turn budget stops every server search.
        iterations: u32::MAX,
        depth,
        replacement_depth,
        damage_rolls,
        consider_crit,
        max_actions_per_player: None,
        ..MctsConfig::default()
    };
    match algorithm {
        BotAlgorithm::Ismcts => {
            let base = IsmctsConfig::default();
            BotSearchConfig::Ismcts(IsmctsConfig {
                search,
                particles: particles.unwrap_or(base.particles),
                ..base
            })
        }
        BotAlgorithm::Mccfr => {
            // `MccfrConfig::default` raises the exploration rate, because the
            // outcome-sampling estimator divides by the selection probability.
            // Keep that rate when this profile replaces the search block.
            let base = MccfrConfig::default();
            BotSearchConfig::Mccfr(MccfrConfig {
                search: MctsConfig {
                    exploration: base.search.exploration,
                    ..search
                },
                particles: particles.unwrap_or(base.particles),
                ..base
            })
        }
        _ => BotSearchConfig::Mcts(search),
    }
}

#[cfg(test)]
mod current_tests {
    use super::*;

    #[test]
    fn an_empty_request_uses_balanced_defaults() {
        let profile = resolve("botP2", &BotProfileRequest::default()).unwrap();
        assert_eq!(profile.view.algorithm, "doubleOracle");
        assert_eq!(
            profile.view.simulation_turn_budget,
            BALANCED_PRESET.simulation_turn_budget
        );
        assert_eq!(profile.view.depth, BALANCED_PRESET.depth);
        assert_eq!(profile.view.damage_rolls, BALANCED_PRESET.damage_rolls);
        assert!(!profile.view.consider_crit);
        match profile.search {
            BotSearchConfig::Exact(config) => assert_eq!(config.max_actions_per_player, None),
            _ => panic!("the default search must be exact"),
        }
    }

    #[test]
    fn raw_limits_reach_a_sampled_search() {
        let request = BotProfileRequest {
            algorithm: Some("ismcts".to_string()),
            simulation_turn_budget: Some(12_345),
            depth: Some(3),
            replacement_depth: Some(2),
            damage_rolls: Some(4),
            consider_crit: Some(true),
            particles: Some(12),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert_eq!(profile.view.simulation_turn_budget, 12_345);
        assert_eq!(profile.view.depth, 3);
        assert_eq!(profile.view.replacement_depth, Some(2));
        assert_eq!(profile.view.damage_rolls, 4);
        assert!(profile.view.consider_crit);
        assert_eq!(profile.view.particles, Some(12));
        match profile.search {
            BotSearchConfig::Ismcts(config) => {
                assert_eq!(config.search.max_actions_per_player, None)
            }
            _ => panic!("the resolved search must use ISMCTS"),
        }
        assert_eq!(profile.search.first_depth(3), 3);
    }

    #[test]
    fn each_preset_has_fixed_limits() {
        for (name, limits) in [
            ("fast", FAST_PRESET),
            ("balanced", BALANCED_PRESET),
            ("competitive", COMPETITIVE_PRESET),
        ] {
            let request = BotProfileRequest {
                preset: Some(name.to_string()),
                algorithm: Some("ismcts".to_string()),
                ..BotProfileRequest::default()
            };
            let profile = resolve("botP2", &request).unwrap();
            // The request names `ismcts`, which never finishes by itself.
            assert_eq!(
                profile.view.simulation_turn_budget,
                limits.sampled_simulation_turn_budget
            );
            assert_eq!(profile.view.depth, limits.depth);
            assert_eq!(profile.view.damage_rolls, limits.damage_rolls);
            assert_eq!(profile.view.particles, Some(limits.particles));
        }
    }

    /// Every preset resolves one turn of lookahead.
    ///
    /// A preset spends its budget on the width of one turn rather than on a
    /// second turn. `PRESET_DEPTH` holds the reason.
    #[test]
    fn every_preset_runs_at_depth_one() {
        for name in ["fast", "balanced", "competitive", "custom"] {
            let request = BotProfileRequest {
                preset: Some(name.to_string()),
                ..BotProfileRequest::default()
            };
            let profile = resolve("botP2", &request).unwrap();
            assert_eq!(profile.view.depth, 1, "{name} must run one turn");
        }
    }

    /// A search that finishes takes a budget that holds one whole solve.
    ///
    /// A search that never finishes takes a much smaller one, because its budget
    /// sets the seconds of one answer instead.
    #[test]
    fn the_budget_of_a_preset_follows_the_algorithm() {
        for name in ["fast", "balanced", "competitive"] {
            let budget = |algorithm: &str| {
                let request = BotProfileRequest {
                    preset: Some(name.to_string()),
                    algorithm: Some(algorithm.to_string()),
                    ..BotProfileRequest::default()
                };
                resolve("botP2", &request)
                    .unwrap()
                    .view
                    .simulation_turn_budget
            };
            let finishing = budget("doubleOracle");
            assert_eq!(finishing, budget("pimc"), "{name}: pimc finishes");
            for sampled in ["ismcts", "mccfr", "mcts"] {
                assert!(
                    budget(sampled) < finishing,
                    "{name}: {sampled} must take the smaller budget"
                );
            }
        }
    }

    /// A wider preset must raise every width, and the budget with them.
    ///
    /// A damage roll and a world both multiply the turns that one answer needs,
    /// so a budget that stays flat would stop the wider preset before it
    /// finishes and would leave it no wider than the preset below.
    #[test]
    fn a_wider_preset_raises_every_width_and_its_budget() {
        let ladder = [FAST_PRESET, BALANCED_PRESET, COMPETITIVE_PRESET];
        for pair in ladder.windows(2) {
            let (lower, upper) = (pair[0], pair[1]);
            assert!(upper.damage_rolls > lower.damage_rolls);
            assert!(upper.particles > lower.particles);
            assert!(upper.simulation_turn_budget > lower.simulation_turn_budget);
            assert!(
                upper.sampled_simulation_turn_budget > lower.sampled_simulation_turn_budget
            );
        }
        assert_eq!(COMPETITIVE_PRESET.damage_rolls, 16, "the widest preset reads every roll");
    }

    #[test]
    fn a_depth_override_does_not_scale_the_budget() {
        let request = BotProfileRequest {
            preset: Some("fast".to_string()),
            algorithm: Some("mcts".to_string()),
            depth: Some(8),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert_eq!(
            profile.view.simulation_turn_budget,
            FAST_PRESET.sampled_simulation_turn_budget
        );
    }

    #[test]
    fn particles_stop_at_the_cap() {
        let request = BotProfileRequest {
            algorithm: Some("mccfr".to_string()),
            particles: Some(MAX_PARTICLES + 1),
            ..BotProfileRequest::default()
        };
        assert!(resolve("analysis", &request).unwrap_err().contains("particles"));
    }

    /// The scaling reads an absent field alone.
    #[test]
    fn a_budget_that_the_client_sends_is_never_scaled() {
        let request = BotProfileRequest {
            algorithm: Some("ismcts".to_string()),
            simulation_turn_budget: Some(1_000),
            depth: Some(8),
            particles: Some(MAX_PARTICLES),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert_eq!(profile.view.simulation_turn_budget, 1_000);
    }

    /// An exact search runs no rollouts, so the depth leaves its default alone.
    #[test]
    fn an_exact_search_keeps_the_flat_default_budget() {
        for depth in [1, 8] {
            let request = BotProfileRequest {
                depth: Some(depth),
                ..BotProfileRequest::default()
            };
            let profile = resolve("botP2", &request).unwrap();
            assert_eq!(
                profile.view.simulation_turn_budget,
                DEFAULT_PRESET.simulation_turn_budget
            );
        }
    }

    #[test]
    fn an_exact_search_starts_the_depth_ladder_at_one() {
        let profile = resolve("botP2", &BotProfileRequest::default()).unwrap();
        assert_eq!(profile.search.first_depth(3), 1);
    }

    #[test]
    fn raw_limits_are_range_checked() {
        let request = BotProfileRequest {
            simulation_turn_budget: Some(0),
            ..BotProfileRequest::default()
        };
        assert!(resolve("botP2", &request)
            .unwrap_err()
            .contains("simulationTurnBudget"));
    }


    /// Each world of a PIMC profile is a complete perfect-information solve, so
    /// the profile carries a `SolveConfig` and a particle count together.
    #[test]
    fn a_pimc_profile_builds_an_exact_search_for_each_world() {
        let request = BotProfileRequest {
            algorithm: Some("pimc".to_string()),
            depth: Some(3),
            damage_rolls: Some(4),
            particles: Some(6),
            seed: Some(11),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();

        assert_eq!(profile.view.algorithm, "pimc");
        // The world draw makes the answer approximate, whatever one world does.
        assert!(!profile.view.exact);
        assert_eq!(profile.view.particles, Some(6));
        assert_eq!(profile.view.seed, Some(11));
        let BotSearchConfig::Pimc(config) = profile.search else {
            panic!("pimc must build a pimc configuration");
        };
        assert_eq!(config.particles, 6);
        assert_eq!(config.solve.depth, 3);
        assert_eq!(config.solve.damage_rolls, 4);
        assert_eq!(config.solve.algorithm, SolverAlgorithm::DoubleOracle);
        assert!(config.solve.iterative_deepening);
        // A ladder would multiply the world count by the depth.
        assert_eq!(profile.search.first_depth(3), 3);
        assert!(profile.search.searches_belief());
    }

    /// The approximation list must name the defect of the method itself.
    #[test]
    fn a_pimc_profile_names_its_strategy_fusion() {
        let request = BotProfileRequest {
            algorithm: Some("pimc".to_string()),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert!(
            profile
                .view
                .approximations
                .iter()
                .any(|line| line.contains("strategy fusion")),
            "{:?}",
            profile.view.approximations
        );
    }

    #[test]
    fn the_default_budget_of_a_pimc_profile_is_fixed() {
        let request = BotProfileRequest {
            algorithm: Some("pimc".to_string()),
            particles: Some(4),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert_eq!(
            profile.view.simulation_turn_budget,
            DEFAULT_PRESET.simulation_turn_budget
        );
    }

    /// Two techniques stay off by decision, for every preset and every
    /// algorithm.
    ///
    /// A cap truncates the action set that the reported strategy ranges over.
    /// The dominance filter reads the current position, so a partner command
    /// that changes the weather or the terrain can invert the comparison it
    /// makes. Both trade an unmeasured amount of answer quality for speed, and
    /// this project buys speed from exact pruning instead.
    ///
    /// A later change that makes a search faster must not reach for either one.
    #[test]
    fn no_profile_caps_or_culls_the_action_set() {
        let algorithms = [
            "doubleOracle",
            "backwardInduction",
            "serializedBounds",
            "mcts",
            "ismcts",
            "mccfr",
            "pimc",
        ];
        for preset in ["fast", "balanced", "competitive", "custom"] {
            for algorithm in algorithms {
                let refine = BotAlgorithm::from_wire("botP2", algorithm)
                    .expect("the name is in the list")
                    .supports_refinement();
                let request = BotProfileRequest {
                    preset: Some(preset.to_string()),
                    algorithm: Some(algorithm.to_string()),
                    refine: refine.then_some(true),
                    ..BotProfileRequest::default()
                };
                let profile = resolve("botP2", &request)
                    .unwrap_or_else(|error| panic!("{preset}/{algorithm}: {error}"));
                let (cap, cull) = match profile.search {
                    BotSearchConfig::Exact(config) => (
                        config.max_actions_per_player,
                        config.prune_dominated_actions,
                    ),
                    BotSearchConfig::Refine { solve, .. } => (
                        solve.max_actions_per_player,
                        solve.prune_dominated_actions,
                    ),
                    BotSearchConfig::Pimc(config) => (
                        config.solve.max_actions_per_player,
                        config.solve.prune_dominated_actions,
                    ),
                    BotSearchConfig::Mcts(config) => (
                        config.max_actions_per_player,
                        config.prune_dominated_actions,
                    ),
                    BotSearchConfig::Ismcts(config) => (
                        config.search.max_actions_per_player,
                        config.search.prune_dominated_actions,
                    ),
                    BotSearchConfig::Mccfr(config) => (
                        config.search.max_actions_per_player,
                        config.search.prune_dominated_actions,
                    ),
                };
                assert_eq!(cap, None, "{preset}/{algorithm} capped the action set");
                assert!(!cull, "{preset}/{algorithm} enabled the dominance filter");
            }
        }
    }

    /// A refined profile must build a refinement search and say so.
    #[test]
    fn a_refined_profile_builds_a_refinement_search() {
        let request = BotProfileRequest {
            algorithm: Some("doubleOracle".to_string()),
            depth: Some(2),
            refine: Some(true),
            ..BotProfileRequest::default()
        };
        let profile = resolve("botP2", &request).unwrap();
        assert!(profile.view.refine);
        let BotSearchConfig::Refine { solve, base_depth } = profile.search else {
            panic!("a refined profile must build a refinement search");
        };
        assert_eq!(solve.depth, 2);
        assert_eq!(base_depth, REFINE_BASE_DEPTH);
        // The pass runs its own base pass, so an outer ladder would repeat it.
        assert!(!solve.iterative_deepening);
        assert_eq!(profile.search.first_depth(2), 2);
        assert!(!profile.search.searches_belief());
        assert!(
            profile
                .view
                .approximations
                .iter()
                .any(|line| line.contains("raises only the")),
            "{:?}",
            profile.view.approximations
        );
    }

    /// Each PIMC world is an exact search, so a world can refine.
    #[test]
    fn a_refined_pimc_profile_refines_each_world() {
        let request = BotProfileRequest {
            algorithm: Some("pimc".to_string()),
            depth: Some(2),
            particles: Some(2),
            refine: Some(true),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert!(profile.view.refine);
        let BotSearchConfig::Pimc(config) = profile.search else {
            panic!("pimc must build a pimc configuration");
        };
        assert_eq!(config.refine_base_depth, Some(REFINE_BASE_DEPTH));
        assert_eq!(config.particles, 2);
        assert!(profile.search.searches_belief());
    }

    /// A sampled search walks trajectories and holds no support to raise.
    #[test]
    fn a_sampled_profile_rejects_refinement() {
        for algorithm in ["mcts", "ismcts", "mccfr"] {
            let request = BotProfileRequest {
                algorithm: Some(algorithm.to_string()),
                refine: Some(true),
                ..BotProfileRequest::default()
            };
            let error = resolve("botP2", &request)
                .expect_err("a sampled profile cannot refine")
                .to_string();
            assert!(error.contains("refine"), "{algorithm}: {error}");
        }
    }

    /// An absent flag keeps the complete search.
    #[test]
    fn an_absent_refine_flag_keeps_the_complete_search() {
        let profile = resolve("botP2", &BotProfileRequest::default()).unwrap();
        assert!(!profile.view.refine);
        assert!(matches!(profile.search, BotSearchConfig::Exact(_)));
    }

    #[test]
    fn old_wire_fields_are_rejected() {
        for body in [
            r#"{"timeMs":1000}"#,
            r#"{"nodeBudget":1000}"#,
            r#"{"iterations":200}"#,
            r#"{"maxActionsPerPlayer":6}"#,
        ] {
            let error = serde_json::from_str::<BotProfileRequest>(body)
                .unwrap_err()
                .to_string();
            assert!(error.contains("unknown field"), "{error}");
        }
    }
}
