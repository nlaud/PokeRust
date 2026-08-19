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
    fn from_wire(scope: &str, name: &str) -> Result<Self, String> {
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

/// The default simulation-turn budget of an exact search.
///
/// It is also the floor of the sampled default that
/// [`default_simulation_turn_budget`] derives.
pub const DEFAULT_SIMULATION_TURN_BUDGET: u64 = 1_000;

/// Rollouts for each particle that the default budget of a sampled search buys.
pub const DEFAULT_ROLLOUTS_PER_PARTICLE: u64 = 500;

/// The ceiling of the derived default.
///
/// The maximum depth and the maximum particle count together derive a budget
/// that runs for minutes. A default stays speed-first, so it stops here. A
/// client that wants more sends the budget itself.
pub const MAX_DEFAULT_SIMULATION_TURN_BUDGET: u64 = 100_000;

pub const DEFAULT_DEPTH: u8 = 2;
pub const DEFAULT_DAMAGE_ROLLS: u8 = 1;
pub const DEFAULT_PARTICLES: usize = 8;

/// The profile as the client sends it.
/// Every field is optional. The server supplies speed-first defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BotProfileRequest {
    /// Defaults to `doubleOracle`.
    ///
    /// `routes::resolve_bot_p2` fills an absent field before this call, because
    /// `doubleOracle` cannot control P2 in a fog-of-war session. A battle
    /// request therefore takes the search of its information mode.
    pub algorithm: Option<String>,
    /// The turn simulations that the whole search may spend.
    ///
    /// An absent field takes the default of
    /// [`default_simulation_turn_budget`], which reads the depth and the
    /// particle count. A field that is present is used as it stands.
    pub simulation_turn_budget: Option<u64>,
    pub depth: Option<u8>,
    pub damage_rolls: Option<u8>,
    pub consider_crit: Option<bool>,
    /// Turns of lookahead below a replacement or a self-switch pivot.
    /// An absent field gives a forced decision the remaining turn depth.
    pub replacement_depth: Option<u8>,
    /// Belief searches only.
    pub particles: Option<usize>,
    /// Makes a sampled search reproducible.
    /// The maximum is JavaScript's largest safe integer.
    pub seed: Option<u64>,
    /// Shows Player 2's strategy to the client. Defaults to false.
    ///
    /// A battle session reads this field. It is the fog-of-war boundary of the
    /// two battle endpoints: without it, no response carries a Player 2 strategy
    /// row. A tracker session already returns both strategies, because the
    /// tracker user typed both rosters, so it ignores the field.
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
        if matches!(self, BotSearchConfig::Exact(_)) {
            1
        } else {
            requested
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

/// The default simulation-turn budget of one resolved profile.
///
/// A sampled search runs until the budget stops it, and one rollout costs about
/// one turn simulation for each turn of depth. One rollout also reads one
/// particle. A flat budget therefore buys fewer rollouts for each particle at
/// every higher depth and at every larger particle count, so both controls look
/// like they make the answer worse. This default holds the rollouts for each
/// particle steady instead.
///
/// An exact search runs no rollouts, so it keeps the flat budget.
///
/// A budget that the client sends is never scaled. `frontend/src/components/
/// solver/solverSettings.ts` repeats this rule, so the interface can show the
/// derived number.
fn default_simulation_turn_budget(exact: bool, depth: u8, particles: Option<usize>) -> u64 {
    if exact {
        return DEFAULT_SIMULATION_TURN_BUDGET;
    }
    let particles = particles.unwrap_or(1).max(1) as u64;
    (DEFAULT_ROLLOUTS_PER_PARTICLE * particles * depth as u64).clamp(
        DEFAULT_SIMULATION_TURN_BUDGET,
        MAX_DEFAULT_SIMULATION_TURN_BUDGET,
    )
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

    let depth = req.depth.unwrap_or(DEFAULT_DEPTH);
    check_range(scope, "depth", depth, 1, 8)?;
    let replacement_depth = match req.replacement_depth {
        Some(value) => {
            check_range(scope, "replacementDepth", value, 1, 8)?;
            Some(value)
        }
        None => None,
    };
    let damage_rolls = req.damage_rolls.unwrap_or(DEFAULT_DAMAGE_ROLLS);
    check_range(scope, "damageRolls", damage_rolls, 1, 16)?;
    let consider_crit = req.consider_crit.unwrap_or(false);
    let particles = match req.particles {
        Some(value) => {
            check_range(scope, "particles", value, 1, 512)?;
            value
        }
        None => DEFAULT_PARTICLES,
    };
    let exact = algorithm.is_exact();
    if let Some(seed) = req.seed {
        check_range(scope, "seed", seed, 0, MAX_SAFE_INTEGER)?;
    }
    let particles = algorithm.uses_particles().then_some(particles);
    // The budget resolves last, because an absent field reads the depth, the
    // particle count, and whether the algorithm samples.
    let simulation_turn_budget = req
        .simulation_turn_budget
        .unwrap_or_else(|| default_simulation_turn_budget(exact, depth, particles));
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
            workers: search_workers(&search),
            particles,
            seed: req.seed,
            reveal_strategy: req.reveal_strategy.unwrap_or(false),
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
fn build_search(
    algorithm: BotAlgorithm,
    depth: u8,
    replacement_depth: Option<u8>,
    particles: Option<usize>,
    damage_rolls: u8,
    consider_crit: bool,
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
        return BotSearchConfig::Exact(exact);
    }
    if algorithm == BotAlgorithm::Pimc {
        // Each world is a complete perfect-information solve, so it takes the
        // configuration that an exact profile builds. `exact_algorithm` gives
        // double oracle to every name that is not exact itself.
        let base = PimcConfig::default();
        return BotSearchConfig::Pimc(PimcConfig {
            solve: exact,
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
    fn an_empty_request_uses_speed_first_defaults() {
        let profile = resolve("botP2", &BotProfileRequest::default()).unwrap();
        assert_eq!(profile.view.algorithm, "doubleOracle");
        assert_eq!(profile.view.simulation_turn_budget, 1_000);
        assert_eq!(profile.view.depth, 2);
        assert_eq!(profile.view.damage_rolls, 1);
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

    /// A sampled search that names no budget keeps its rollout count as the
    /// depth grows. A flat budget would divide the same turns over deeper paths.
    #[test]
    fn the_default_budget_of_a_sampled_search_grows_with_the_depth() {
        let mut previous = 0;
        for depth in 1..=8 {
            let request = BotProfileRequest {
                algorithm: Some("mcts".to_string()),
                depth: Some(depth),
                ..BotProfileRequest::default()
            };
            let budget = resolve("botP2", &request).unwrap().view.simulation_turn_budget;
            // The floor holds the two shallowest depths at the flat budget.
            assert_eq!(
                budget,
                (DEFAULT_ROLLOUTS_PER_PARTICLE * depth as u64).max(DEFAULT_SIMULATION_TURN_BUDGET)
            );
            assert!(budget >= previous, "depth {depth} must not lower the budget");
            previous = budget;
        }
        assert!(previous > DEFAULT_SIMULATION_TURN_BUDGET, "depth 8 must raise it");
    }

    /// One rollout reads one particle, so a larger set needs more rollouts to
    /// give each particle the same number of visits.
    #[test]
    fn the_default_budget_of_a_belief_search_grows_with_the_particles() {
        let request = BotProfileRequest {
            algorithm: Some("ismcts".to_string()),
            particles: Some(8),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert_eq!(profile.view.particles, Some(8));
        assert_eq!(
            profile.view.simulation_turn_budget,
            DEFAULT_ROLLOUTS_PER_PARTICLE * 8 * DEFAULT_DEPTH as u64
        );
    }

    /// The derived default stays speed-first at the largest limits.
    #[test]
    fn the_derived_default_stops_at_the_ceiling() {
        let request = BotProfileRequest {
            algorithm: Some("mccfr".to_string()),
            depth: Some(8),
            particles: Some(512),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert_eq!(
            profile.view.simulation_turn_budget,
            MAX_DEFAULT_SIMULATION_TURN_BUDGET
        );
    }

    /// The scaling reads an absent field alone.
    #[test]
    fn a_budget_that_the_client_sends_is_never_scaled() {
        let request = BotProfileRequest {
            algorithm: Some("ismcts".to_string()),
            simulation_turn_budget: Some(1_000),
            depth: Some(8),
            particles: Some(64),
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
                DEFAULT_SIMULATION_TURN_BUDGET
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

    /// One rollout of a sampled search reads one particle, and one world of a
    /// PIMC search takes one share of the budget. Both scale the same way.
    #[test]
    fn the_default_budget_of_a_pimc_profile_grows_with_the_particles() {
        let request = BotProfileRequest {
            algorithm: Some("pimc".to_string()),
            particles: Some(4),
            ..BotProfileRequest::default()
        };
        let profile = resolve("analysis", &request).unwrap();
        assert_eq!(
            profile.view.simulation_turn_budget,
            DEFAULT_ROLLOUTS_PER_PARTICLE * 4 * DEFAULT_DEPTH as u64
        );
    }

    #[test]
    fn old_wire_fields_are_rejected() {
        for body in [
            r#"{"preset":"fast"}"#,
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
