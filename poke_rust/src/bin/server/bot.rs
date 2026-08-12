//! One solver profile of a session.
//!
//! A create-battle request can carry one profile, and a tracker analysis
//! request carries the same shape. The profile names one search algorithm and
//! one preset. The preset supplies every limit, and an explicit field of the
//! request overrides the preset value.
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

use std::time::Duration;

use serde::{Deserialize, Serialize};

use poke_rust::solver::ismcts::IsmctsConfig;
use poke_rust::solver::mccfr::MccfrConfig;
use poke_rust::solver::mcts::MctsConfig;
use poke_rust::solver::{SolveConfig, SolverAlgorithm};

/// The search that a P2 bot profile configures.
///
/// The first three names solve the game exactly to the depth horizon. The last
/// three names sample, so they return an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotAlgorithm {
    BackwardInduction,
    SerializedBounds,
    DoubleOracle,
    Mcts,
    Ismcts,
    Mccfr,
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
    /// Only these two algorithms read the `particles` limit.
    fn uses_particles(self) -> bool {
        matches!(self, BotAlgorithm::Ismcts | BotAlgorithm::Mccfr)
    }

    fn exact_algorithm(self) -> SolverAlgorithm {
        match self {
            BotAlgorithm::SerializedBounds => SolverAlgorithm::SerializedBounds,
            BotAlgorithm::BackwardInduction => SolverAlgorithm::BackwardInduction,
            _ => SolverAlgorithm::DoubleOracle,
        }
    }
}

/// A named group of limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotPreset {
    Fast,
    Balanced,
    Strong,
}

impl BotPreset {
    fn from_wire(scope: &str, name: &str) -> Result<Self, String> {
        match name {
            "fast" => Ok(BotPreset::Fast),
            "balanced" => Ok(BotPreset::Balanced),
            "strong" => Ok(BotPreset::Strong),
            other => Err(format!("{scope}.preset: unknown preset {other:?}")),
        }
    }

    fn wire_name(self) -> &'static str {
        match self {
            BotPreset::Fast => "fast",
            BotPreset::Balanced => "balanced",
            BotPreset::Strong => "strong",
        }
    }
}

/// Every limit of one preset.
///
/// `time_ms` bounds the whole analysis job. An exact search also maps it to
/// [`SolveConfig::deadline`]. The sampling searches hold no deadline field, so
/// the job enforces the limit around the search.
#[derive(Debug, Clone, Copy)]
struct PresetLimits {
    time_ms: u64,
    node_budget: u64,
    depth: u8,
    iterations: u32,
    particles: usize,
    max_actions_per_player: Option<usize>,
}

/// The largest integer that JavaScript can represent without precision loss.
/// `analysis::random_seed` draws inside this range for the same reason.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The limits of each preset.
///
/// The interactive move limit is 45 seconds, so the strong preset stays under
/// it. `TODO.md` records that limit.
fn preset_limits(preset: BotPreset) -> PresetLimits {
    match preset {
        BotPreset::Fast => PresetLimits {
            time_ms: 2_000,
            node_budget: 50_000,
            depth: 1,
            iterations: 200,
            particles: 8,
            max_actions_per_player: Some(6),
        },
        BotPreset::Balanced => PresetLimits {
            time_ms: 10_000,
            node_budget: 500_000,
            depth: 2,
            iterations: 2_000,
            particles: 16,
            max_actions_per_player: Some(12),
        },
        BotPreset::Strong => PresetLimits {
            time_ms: 40_000,
            node_budget: 4_000_000,
            depth: 3,
            iterations: 20_000,
            particles: 32,
            max_actions_per_player: None,
        },
    }
}

/// The profile as the client sends it.
/// Every field is optional, and the preset fills each absent field.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BotProfileRequest {
    /// Defaults to `doubleOracle`.
    pub algorithm: Option<String>,
    /// Defaults to `balanced`.
    pub preset: Option<String>,
    pub time_ms: Option<u64>,
    /// Exact algorithms only.
    pub node_budget: Option<u64>,
    pub depth: Option<u8>,
    /// The search is serial, so the server accepts only 1.
    pub workers: Option<u8>,
    /// Sampling algorithms only.
    pub iterations: Option<u32>,
    /// Belief searches only.
    pub particles: Option<usize>,
    /// Makes a sampled search reproducible.
    /// The maximum is JavaScript's largest safe integer.
    pub seed: Option<u64>,
    pub max_actions_per_player: Option<usize>,
}

/// The resolved profile as the client reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BotProfileView {
    pub algorithm: String,
    pub preset: String,
    /// True when the algorithm itself is exact.
    /// A limit can still make the result approximate.
    pub exact: bool,
    pub time_ms: Option<u64>,
    pub node_budget: Option<u64>,
    pub depth: u8,
    pub workers: u8,
    pub iterations: Option<u32>,
    pub particles: Option<usize>,
    pub seed: Option<u64>,
    pub max_actions_per_player: Option<usize>,
    /// Each reason that the result can differ from the exact answer.
    /// The interface shows this list.
    pub approximations: Vec<String>,
    /// Each knob that the server changed away from the preset value.
    pub adjustments: Vec<String>,
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
}

impl BotSearchConfig {
    /// The same configuration at another depth horizon.
    ///
    /// The tracker ladder in `tracker_analysis.rs` runs one rung for each depth
    /// from one through the configured depth, so it needs this copy.
    pub fn with_depth(self, depth: u8) -> BotSearchConfig {
        match self {
            BotSearchConfig::Exact(config) => BotSearchConfig::Exact(SolveConfig {
                depth,
                ..config
            }),
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
        }
    }

    /// The same configuration with a new wall-clock deadline.
    ///
    /// Only an exact search reads a deadline. A sampling search holds no
    /// deadline field, so the caller bounds it around the search.
    pub fn with_deadline(self, deadline: Duration) -> BotSearchConfig {
        match self {
            BotSearchConfig::Exact(config) => BotSearchConfig::Exact(SolveConfig {
                deadline: Some(deadline),
                ..config
            }),
            other => other,
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

/// Validates one request and resolves it against its preset.
///
/// `scope` names the request field in every error line. The battle endpoint
/// passes `botP2`, and the tracker analysis endpoint passes `analysis`.
///
/// `damage_rolls` and `consider_crit` come from the caller, so the search uses
/// the same physics as the session that asked for it.
pub fn resolve(
    scope: &str,
    req: &BotProfileRequest,
    damage_rolls: u8,
    consider_crit: bool,
) -> Result<BotProfile, String> {
    let algorithm = match &req.algorithm {
        Some(name) => BotAlgorithm::from_wire(scope, name)?,
        None => BotAlgorithm::DoubleOracle,
    };
    let preset = match &req.preset {
        Some(name) => BotPreset::from_wire(scope, name)?,
        None => BotPreset::Balanced,
    };
    let limits = preset_limits(preset);

    // Reject a limit that the selected algorithm cannot read. A silent drop
    // would show the client a profile that the search never used.
    reject_unused(
        scope,
        req.node_budget.is_some() && !algorithm.is_exact(),
        "nodeBudget",
        "a node budget applies only to an exact algorithm",
    )?;
    reject_unused(
        scope,
        req.iterations.is_some() && algorithm.is_exact(),
        "iterations",
        "an iteration count applies only to a sampling algorithm",
    )?;
    reject_unused(
        scope,
        req.particles.is_some() && !algorithm.uses_particles(),
        "particles",
        "a particle count applies only to ismcts or mccfr",
    )?;
    reject_unused(
        scope,
        req.seed.is_some() && algorithm.is_exact(),
        "seed",
        "a seed applies only to a sampling algorithm",
    )?;

    if req.workers.is_some_and(|workers| workers != 1) {
        return Err(format!(
            "{scope}.workers: the search is serial, so only 1 worker is available"
        ));
    }

    let mut adjustments = Vec::new();
    let mut note = |changed: bool, field: &str, value: String| {
        if !changed {
            return;
        }
        adjustments.push(format!(
            "{field} overrides the {} preset: {value}",
            preset.wire_name()
        ));
    };

    let time_ms = match req.time_ms {
        Some(value) => {
            check_range(scope, "timeMs", value, 1, 600_000)?;
            note(value != limits.time_ms, "timeMs", value.to_string());
            value
        }
        None => limits.time_ms,
    };
    let depth = match req.depth {
        Some(value) => {
            check_range(scope, "depth", value, 1, 8)?;
            note(value != limits.depth, "depth", value.to_string());
            value
        }
        None => limits.depth,
    };
    let node_budget = match req.node_budget {
        Some(value) => {
            check_range(scope, "nodeBudget", value, 1, 1_000_000_000)?;
            note(value != limits.node_budget, "nodeBudget", value.to_string());
            value
        }
        None => limits.node_budget,
    };
    let iterations = match req.iterations {
        Some(value) => {
            check_range(scope, "iterations", value, 1, 1_000_000)?;
            note(value != limits.iterations, "iterations", value.to_string());
            value
        }
        None => limits.iterations,
    };
    let particles = match req.particles {
        Some(value) => {
            check_range(scope, "particles", value, 1, 512)?;
            note(value != limits.particles, "particles", value.to_string());
            value
        }
        None => limits.particles,
    };
    let max_actions_per_player = match req.max_actions_per_player {
        Some(value) => {
            check_range(scope, "maxActionsPerPlayer", value, 1, 1_000)?;
            note(
                Some(value) != limits.max_actions_per_player,
                "maxActionsPerPlayer",
                value.to_string(),
            );
            Some(value)
        }
        None => limits.max_actions_per_player,
    };

    let exact = algorithm.is_exact();
    if let Some(seed) = req.seed {
        check_range(scope, "seed", seed, 0, MAX_SAFE_INTEGER)?;
    }
    let node_budget = exact.then_some(node_budget);
    let iterations = (!exact).then_some(iterations);
    let particles = algorithm.uses_particles().then_some(particles);

    let mut approximations = Vec::new();
    approximations.push(format!(
        "The search stops after {depth} turn(s) and scores the position with the leaf evaluator."
    ));
    if !exact {
        approximations
            .push("The search samples trajectories, so the strategy is an estimate.".to_string());
    }
    if let Some(count) = particles {
        approximations.push(format!(
            "The search draws {count} world(s) from the belief, so hidden data is sampled."
        ));
    }
    if let Some(cap) = max_actions_per_player {
        approximations.push(format!(
            "Each player keeps at most {cap} action(s), so the search can miss an action."
        ));
    }
    if let Some(budget) = node_budget {
        approximations.push(format!(
            "The {budget}-node budget can stop a deeper pass. The bot then uses the last complete depth or a partial first pass."
        ));
    }
    if exact {
        approximations.push(format!(
            "The {time_ms} ms limit can stop a deeper pass. The bot then uses the last complete depth or a partial first pass."
        ));
    } else {
        approximations.push(format!("The search has a {time_ms} ms limit."));
    }

    let search = build_search(
        algorithm,
        depth,
        time_ms,
        node_budget,
        iterations,
        particles,
        max_actions_per_player,
        damage_rolls,
        consider_crit,
    );

    Ok(BotProfile {
        view: BotProfileView {
            algorithm: algorithm.wire_name().to_string(),
            preset: preset.wire_name().to_string(),
            exact,
            time_ms: Some(time_ms),
            node_budget,
            depth,
            workers: 1,
            iterations,
            particles,
            seed: req.seed,
            max_actions_per_player,
            approximations,
            adjustments,
        },
        search,
    })
}

/// Builds the solver configuration of one resolved profile.
#[allow(clippy::too_many_arguments)]
fn build_search(
    algorithm: BotAlgorithm,
    depth: u8,
    time_ms: u64,
    node_budget: Option<u64>,
    iterations: Option<u32>,
    particles: Option<usize>,
    max_actions_per_player: Option<usize>,
    damage_rolls: u8,
    consider_crit: bool,
) -> BotSearchConfig {
    if algorithm.is_exact() {
        return BotSearchConfig::Exact(SolveConfig {
            depth,
            iterative_deepening: true,
            damage_rolls,
            consider_crit,
            algorithm: algorithm.exact_algorithm(),
            max_actions_per_player,
            node_budget,
            deadline: Some(Duration::from_millis(time_ms)),
            ..SolveConfig::default()
        });
    }

    let search = MctsConfig {
        iterations: iterations.unwrap_or_else(|| MctsConfig::default().iterations),
        depth,
        damage_rolls,
        consider_crit,
        max_actions_per_player,
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
mod tests {
    use super::*;

    fn request(algorithm: &str, preset: &str) -> BotProfileRequest {
        BotProfileRequest {
            algorithm: Some(algorithm.to_string()),
            preset: Some(preset.to_string()),
            ..BotProfileRequest::default()
        }
    }

    #[test]
    fn each_preset_resolves_to_its_limits() {
        for (name, depth, budget, time) in [
            ("fast", 1, 50_000, 2_000),
            ("balanced", 2, 500_000, 10_000),
            ("strong", 3, 4_000_000, 40_000),
        ] {
            let profile = resolve("botP2", &request("doubleOracle", name), 16, true).unwrap();
            assert_eq!(profile.view.depth, depth);
            assert_eq!(profile.view.node_budget, Some(budget));
            assert_eq!(profile.view.time_ms, Some(time));
            assert_eq!(profile.view.workers, 1);
            assert!(profile.view.adjustments.is_empty());
        }
    }

    #[test]
    fn an_override_replaces_the_preset_value_and_adds_one_adjustment() {
        let mut req = request("doubleOracle", "fast");
        req.depth = Some(4);
        let profile = resolve("botP2", &req, 16, true).unwrap();
        assert_eq!(profile.view.depth, 4);
        assert_eq!(profile.view.adjustments.len(), 1);
        assert!(profile.view.adjustments[0].contains("depth"));
        assert!(profile.view.adjustments[0].contains("fast"));
    }

    #[test]
    fn values_equal_to_the_preset_are_not_adjustments() {
        let mut req = request("ismcts", "fast");
        req.time_ms = Some(2_000);
        req.depth = Some(1);
        req.iterations = Some(200);
        req.particles = Some(8);
        req.max_actions_per_player = Some(6);

        let profile = resolve("botP2", &req, 16, true).unwrap();

        assert!(profile.view.adjustments.is_empty());

        let mut req = request("doubleOracle", "fast");
        req.time_ms = Some(2_000);
        req.depth = Some(1);
        req.node_budget = Some(50_000);
        req.max_actions_per_player = Some(6);

        let profile = resolve("botP2", &req, 16, true).unwrap();

        assert!(profile.view.adjustments.is_empty());
    }

    #[test]
    fn a_sampling_limit_on_an_exact_algorithm_is_rejected() {
        let mut req = request("doubleOracle", "fast");
        req.iterations = Some(100);
        let error = resolve("botP2", &req, 16, true).unwrap_err();
        assert!(error.contains("iterations"));

        let mut req = request("ismcts", "fast");
        req.node_budget = Some(100);
        let error = resolve("botP2", &req, 16, true).unwrap_err();
        assert!(error.contains("nodeBudget"));

        let mut req = request("mcts", "fast");
        req.particles = Some(4);
        let error = resolve("botP2", &req, 16, true).unwrap_err();
        assert!(error.contains("particles"));
    }

    #[test]
    fn a_second_worker_is_rejected() {
        let mut req = request("doubleOracle", "fast");
        req.workers = Some(2);
        let error = resolve("botP2", &req, 16, true).unwrap_err();
        assert!(error.contains("serial"));
    }

    #[test]
    fn an_exact_algorithm_rejects_a_seed() {
        let mut req = request("doubleOracle", "fast");
        req.seed = Some(7);

        let error = resolve("botP2", &req, 16, true).unwrap_err();

        assert!(error.contains("seed"));
        assert!(error.contains("sampling"));
    }

    #[test]
    fn an_out_of_range_limit_is_rejected() {
        let mut req = request("doubleOracle", "fast");
        req.depth = Some(0);
        assert!(resolve("botP2", &req, 16, true).unwrap_err().contains("depth"));

        let mut req = request("ismcts", "fast");
        req.particles = Some(10_000);
        assert!(resolve("botP2", &req, 16, true).unwrap_err().contains("particles"));

        let mut req = request("mcts", "fast");
        req.seed = Some(MAX_SAFE_INTEGER + 1);
        assert!(resolve("botP2", &req, 16, true).unwrap_err().contains("seed"));
    }

    #[test]
    fn an_unknown_name_is_rejected() {
        let mut req = request("minimax", "fast");
        assert!(resolve("botP2", &req, 16, true).unwrap_err().contains("algorithm"));
        req = request("doubleOracle", "instant");
        assert!(resolve("botP2", &req, 16, true).unwrap_err().contains("preset"));
    }

    #[test]
    fn each_algorithm_builds_its_own_configuration() {
        let cases = [
            ("backwardInduction", "exact"),
            ("serializedBounds", "exact"),
            ("doubleOracle", "exact"),
            ("mcts", "mcts"),
            ("ismcts", "ismcts"),
            ("mccfr", "mccfr"),
        ];
        for (name, want) in cases {
            let profile = resolve("botP2", &request(name, "balanced"), 16, true).unwrap();
            let got = match profile.search {
                BotSearchConfig::Exact(_) => "exact",
                BotSearchConfig::Mcts(_) => "mcts",
                BotSearchConfig::Ismcts(_) => "ismcts",
                BotSearchConfig::Mccfr(_) => "mccfr",
            };
            assert_eq!(got, want, "{name}");
            assert_eq!(profile.view.algorithm, name);
        }
    }

    #[test]
    fn the_exact_algorithms_carry_their_solver_algorithm() {
        let profile = resolve("botP2", &request("serializedBounds", "fast"), 16, true).unwrap();
        let BotSearchConfig::Exact(config) = profile.search else {
            panic!("serializedBounds must build an exact configuration");
        };
        assert_eq!(config.algorithm, SolverAlgorithm::SerializedBounds);
        assert_eq!(config.depth, 1);
        assert!(config.iterative_deepening);
        assert_eq!(config.node_budget, Some(50_000));
        assert_eq!(config.deadline, Some(Duration::from_millis(2_000)));
        assert_eq!(config.damage_rolls, 16);
        assert!(config.consider_crit);
    }

    #[test]
    fn a_sampling_profile_carries_its_counts() {
        let profile = resolve("botP2", &request("mccfr", "strong"), 8, false).unwrap();
        let BotSearchConfig::Mccfr(config) = profile.search else {
            panic!("mccfr must build an mccfr configuration");
        };
        assert_eq!(config.search.iterations, 20_000);
        assert_eq!(config.particles, 32);
        assert_eq!(config.search.damage_rolls, 8);
        assert_eq!(
            config.search.exploration,
            MccfrConfig::default().search.exploration
        );
        assert_eq!(profile.view.iterations, Some(20_000));
        assert_eq!(profile.view.node_budget, None);
    }

    #[test]
    fn an_exact_profile_reports_only_its_own_approximations() {
        let profile = resolve("botP2", &request("doubleOracle", "strong"), 16, true).unwrap();
        // The strong preset keeps every action, so no action-cap line appears.
        assert_eq!(profile.view.max_actions_per_player, None);
        assert!(profile.view.exact);
        let joined = profile.view.approximations.join("\n");
        assert!(!joined.contains("action(s)"));
        assert!(joined.contains("leaf evaluator"));
        assert!(joined.contains("node budget"));
        assert!(joined.contains("ms"));
    }

    #[test]
    fn a_sampling_profile_reports_its_sampling() {
        let profile = resolve("botP2", &request("ismcts", "balanced"), 16, true).unwrap();
        assert!(!profile.view.exact);
        let joined = profile.view.approximations.join("\n");
        assert!(joined.contains("samples trajectories"));
        assert!(joined.contains("world(s) from the belief"));
    }

    /// Each caller names its own request field, so an error line must point at
    /// the field that the client actually sent.
    #[test]
    fn the_scope_names_the_request_field_of_every_error() {
        let mut req = request("minimax", "fast");
        assert!(
            resolve("analysis", &req, 16, true)
                .unwrap_err()
                .starts_with("analysis.algorithm")
        );

        req = request("doubleOracle", "instant");
        assert!(
            resolve("analysis", &req, 16, true)
                .unwrap_err()
                .starts_with("analysis.preset")
        );

        req = request("doubleOracle", "fast");
        req.depth = Some(0);
        assert!(
            resolve("analysis", &req, 16, true)
                .unwrap_err()
                .starts_with("analysis.depth")
        );

        req = request("doubleOracle", "fast");
        req.iterations = Some(100);
        assert!(
            resolve("analysis", &req, 16, true)
                .unwrap_err()
                .starts_with("analysis.iterations")
        );

        req = request("doubleOracle", "fast");
        req.workers = Some(2);
        assert!(
            resolve("analysis", &req, 16, true)
                .unwrap_err()
                .starts_with("analysis.workers")
        );
    }

    /// The ladder runs one rung for each depth, so the copy must change the
    /// depth of every variant and leave every other limit in place.
    #[test]
    fn with_depth_changes_only_the_depth_of_each_variant() {
        for (name, want_iterations) in [
            ("doubleOracle", None),
            ("mcts", Some(2_000)),
            ("ismcts", Some(2_000)),
            ("mccfr", Some(2_000)),
        ] {
            let profile = resolve("botP2", &request(name, "balanced"), 16, true).unwrap();
            match profile.search.with_depth(1) {
                BotSearchConfig::Exact(config) => {
                    assert_eq!(config.depth, 1, "{name}");
                    assert_eq!(config.node_budget, Some(500_000), "{name}");
                    assert_eq!(config.algorithm, SolverAlgorithm::DoubleOracle, "{name}");
                }
                BotSearchConfig::Mcts(config) => {
                    assert_eq!(config.depth, 1, "{name}");
                    assert_eq!(Some(config.iterations), want_iterations, "{name}");
                }
                BotSearchConfig::Ismcts(config) => {
                    assert_eq!(config.search.depth, 1, "{name}");
                    assert_eq!(config.particles, 16, "{name}");
                }
                BotSearchConfig::Mccfr(config) => {
                    assert_eq!(config.search.depth, 1, "{name}");
                    assert_eq!(config.particles, 16, "{name}");
                    assert_eq!(
                        config.search.exploration,
                        MccfrConfig::default().search.exploration,
                        "{name}"
                    );
                }
            }
        }
    }

    /// Only an exact search reads a deadline, so the copy must leave a sampling
    /// configuration alone.
    #[test]
    fn with_deadline_changes_the_exact_configuration_only() {
        let exact = resolve("botP2", &request("doubleOracle", "balanced"), 16, true).unwrap();
        let BotSearchConfig::Exact(config) = exact.search.with_deadline(Duration::from_millis(25))
        else {
            panic!("doubleOracle must build an exact configuration");
        };
        assert_eq!(config.deadline, Some(Duration::from_millis(25)));

        let sampled = resolve("botP2", &request("ismcts", "balanced"), 16, true).unwrap();
        let BotSearchConfig::Ismcts(before) = sampled.search else {
            panic!("ismcts must build an ismcts configuration");
        };
        let BotSearchConfig::Ismcts(after) =
            sampled.search.with_deadline(Duration::from_millis(25))
        else {
            panic!("a deadline must not change the variant");
        };
        assert_eq!(after.search.iterations, before.search.iterations);
        assert_eq!(after.particles, before.particles);
    }

    #[test]
    fn an_empty_request_uses_the_default_names() {
        let profile = resolve("botP2", &BotProfileRequest::default(), 16, true).unwrap();
        assert_eq!(profile.view.algorithm, "doubleOracle");
        assert_eq!(profile.view.preset, "balanced");
        assert_eq!(profile.view.seed, None);
    }
}
