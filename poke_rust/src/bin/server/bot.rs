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

    /// True when the search builds every branch of a turn before it descends.
    ///
    /// This is the split that decides what a damage roll costs, and the two
    /// families answer it differently.
    ///
    /// An exact search, `pimc`, and `mcts` resolve a turn with
    /// `TransitionMode::Enumerated`. They build every branch and then read or
    /// draw from the set, so each damage roll multiplies the work.
    ///
    /// `ismcts` and `mccfr` ignore `MctsConfig::transition` and always call
    /// `sample_transition`, which draws one outcome without building the rest.
    /// A damage roll only widens the set that the one draw comes from.
    /// `IsmctsConfig` records that it ignores the field.
    ///
    /// The measured gap is two orders of magnitude. Sixteen rolls cost `ismcts`
    /// 1.61 times one roll, and they cost `mcts` 3,400 times one roll.
    fn enumerates_turn_branches(self) -> bool {
        !matches!(self, BotAlgorithm::Ismcts | BotAlgorithm::Mccfr)
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
///
/// Each limit below comes in a pair, because the two search families have
/// different cost models and one number cannot serve both. Read
/// [`BotAlgorithm::enumerates_turn_branches`] for the split itself.
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
    /// The depth of a search that enumerates every branch of a turn.
    ///
    /// Such a search multiplies its tree by the branch count of a turn for each
    /// ply, so this stays at [`PRESET_DEPTH`].
    pub depth: u8,
    /// The depth of a belief search.
    ///
    /// One iteration of such a search costs one `sample_transition` for each
    /// ply, so depth is a linear cost here and not an exponential one. A
    /// fog-of-war preset can therefore afford lookahead that an exact preset
    /// cannot, and lookahead is what dilutes the error of the leaf evaluator.
    pub sampled_depth: u8,
    /// The damage rolls of a search that enumerates every branch of a turn.
    ///
    /// Each roll multiplies the branch set. `benches/RESULTS.md` records that a
    /// doubles solve rises 93 times from one roll to sixteen while the value
    /// moves by 0.005. Keep this number small.
    pub exact_damage_rolls: u8,
    /// The damage rolls of a belief search.
    ///
    /// Such a search draws one outcome for each turn whatever this number is, so
    /// a roll widens the set that the draw comes from and does not multiply the
    /// work. The measured cost is 1.61 times between one roll and sixteen,
    /// against 3,400 times for `mcts` over the same range.
    ///
    /// Keep this number at the full sixteen. One roll makes every attack deal
    /// its average damage, so the search cannot see a roll that faints a target
    /// and a roll that does not. A doubles bot decides on that threshold.
    pub sampled_damage_rolls: u8,
    /// The worlds that a belief search draws.
    ///
    /// `pimc` solves each world completely, so `pimc_particles` lowers this
    /// number to what the budget can finish. `ismcts` and `mccfr` share one
    /// budget across the set, so they read it without a change.
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

    /// The depth that one algorithm takes from this preset.
    pub fn depth_for_algorithm(&self, algorithm: BotAlgorithm) -> u8 {
        if algorithm.enumerates_turn_branches() {
            self.depth
        } else {
            self.sampled_depth
        }
    }

    /// The damage rolls that one algorithm takes from this preset.
    pub fn damage_rolls_for_algorithm(&self, algorithm: BotAlgorithm) -> u8 {
        if algorithm.enumerates_turn_branches() {
            self.exact_damage_rolls
        } else {
            self.sampled_damage_rolls
        }
    }

    /// The worlds that one algorithm takes from this preset.
    ///
    /// `pimc` solves each world completely, so a count above what the budget can
    /// finish advertises worlds that the search never reaches. Every other
    /// belief search shares one budget across the whole set, so the count
    /// changes the spread of its sampling and not its cost.
    pub fn particles_for_algorithm(&self, algorithm: BotAlgorithm) -> usize {
        if algorithm == BotAlgorithm::Pimc {
            self.pimc_particles()
        } else {
            self.particles
        }
    }

    /// The worlds that the budget of this preset can solve.
    ///
    /// Each world of `pimc` is a complete solve. `pimc::first_world_quota` gives
    /// world one the larger of the even share and one half of the budget, so a
    /// large count does not make the search reach more worlds. It makes the
    /// search report a particle count that it never reached.
    ///
    /// The budget holds `EXACT_SOLVE_HEADROOM` measured solves, so that is the
    /// count that fits. Two is the floor, because `pimc::GUARANTEED_WORLDS`
    /// promises two searched worlds.
    fn pimc_particles(&self) -> usize {
        self.particles.min(EXACT_SOLVE_HEADROOM as usize).max(2)
    }
}

/// The depth that an enumerating preset runs.
///
/// A depth of one resolves the turn and scores each outcome with the leaf
/// evaluator. Depth two costs a whole depth-one solve for each cell of the root
/// matrix, and `benches/RESULTS.md` records that a doubles position needs about
/// eight minutes for one round of it.
///
/// This depth binds the exact searches, `pimc`, and `mcts`. A belief search runs
/// at [`SAMPLED_PRESET_DEPTH`], because its cost in depth is linear.
///
/// A request can still ask for a deeper search. `SolverControls` offers the
/// field, and `refine` reaches a deeper cell without a complete deep pass.
pub const PRESET_DEPTH: u8 = 1;

/// The depth that a belief preset runs.
///
/// `ismcts` and `mccfr` descend one sampled path for each iteration, so one more
/// ply costs one more `sample_transition` and not a whole subtree. One turn
/// budget therefore buys about the same seconds at every depth. It buys fewer
/// and deeper iterations instead.
///
/// `benches/RESULTS.md` measured 100,000 turns of the doubles pairing at each
/// depth, and this is what 30 seconds buys.
///
/// | Depth | Iterations in 30s |
/// |---|---|
/// | 1 | 36,800 |
/// | 2 | 18,200 |
/// | 3 | 12,100 |
/// | 4 | 9,000 |
///
/// Lookahead is the one thing that dilutes the error of the leaf evaluator.
/// `TODO.md` records that error at about 0.10 of win probability, which is wider
/// than most real edges, so a fog-of-war preset spends its budget here.
///
/// Depth stops at two because the other side of the trade also matters. A
/// doubles root offers about 290 actions against 370. Depth 3 leaves about 42
/// visits for each root action, and depth 2 leaves about 63.
///
/// No measurement says which of the two plays better. `TODO.md` item 1 adds the
/// bench that can answer it. Until that bench exists, take the depth that keeps
/// more visits. Depth 1 is worse than both, because it makes every number the
/// leaf evaluator through one turn.
pub const SAMPLED_PRESET_DEPTH: u8 = 2;

/// The depth that a refined profile solves completely before it raises cells.
///
/// One turn of lookahead over the complete action set is the answer that every
/// later round improves. A doubles position finishes it in about one third of a
/// second at one damage roll.
///
/// This equals [`PRESET_DEPTH`], so a preset at that depth has nothing to raise.
/// `resolve` rejects `refine` unless the request asks for a deeper depth.
pub const REFINE_BASE_DEPTH: u8 = 1;

/// The largest particle count that a request can ask for.
///
/// Each world of `pimc` is a complete solve, so this count multiplies the cost
/// of that search directly. `ismcts` and `mccfr` draw the same number of worlds
/// and share one budget across them, so the count changes the spread of their
/// sampling and not their cost.
pub const MAX_PARTICLES: usize = 64;

/// The measured solves that a finishing budget must hold.
///
/// The sweep below measures the cheapest doubles pairing that `depth2_cost`
/// reports. A costlier pairing needs more, and `pimc` needs one budget for each
/// world that it searches. Four covers both.
const EXACT_SOLVE_HEADROOM: u64 = 4;

/// The budget of a search that finishes, from the rolls that the preset reads.
///
/// `cargo bench --bench depth1_budget` measured one depth-one solve of the
/// cheapest doubles pairing, at 290 actions against 370.
///
/// | Damage rolls | Turn simulations | Time on 22 workers |
/// |---|---|---|
/// | 1 | 14,482 | 0.36s |
/// | 2 | 33,226 | 1.71s |
/// | 3 | 105,432 | 7.17s |
/// | 4 | 268,912 | 25.32s |
///
/// The exact rolls of each preset come from that table and from one rule: the
/// slowest pairing must answer inside [`LIVE_TURN_SECONDS`]. The measured value
/// moved by 0.005 across the whole roll sweep, so a low roll count costs almost
/// no accuracy and buys the whole time budget.
///
/// Singles never reaches these numbers. The same sweep measured 432 turn
/// simulations for a singles position at sixteen rolls, so the budget binds on
/// doubles alone.
const fn budget_for(measured_doubles_solve: u64) -> u64 {
    measured_doubles_solve * EXACT_SOLVE_HEADROOM
}

/// The budget of a belief search, from the seconds that one answer can take.
///
/// A belief search spends every turn that it gets, so no budget makes it
/// complete. The budget is the clock.
///
/// `sampled_rate` and `belief_rate` in the same bench measure the two families
/// apart, because they do not share a rate. At sixteen damage rolls `mcts` runs
/// 2 turns for each second and `ismcts` runs 2,189. A preset that sized the
/// fog-of-war budget from the `mcts` rate therefore set the wrong clock.
const fn sampled_budget_for(turns_for_each_second: u64, seconds: u64) -> u64 {
    turns_for_each_second * seconds
}

/// The turn simulations that a belief search runs in one second.
///
/// `belief_depth_scaling` in `depth1_budget` measured 100,000 turns in 53.36
/// seconds. That row runs the configuration of a preset: depth
/// [`SAMPLED_PRESET_DEPTH`], 16 damage rolls, and 24 worlds.
///
/// Measure this rate at the configuration that the preset runs. The rate falls
/// with the damage rolls and with the depth, so a rate from another row sets the
/// wrong clock. A depth-1 measurement reads 2,189 turns for each second, and a
/// budget from that number would run 22 seconds long.
///
/// A belief search runs on one thread. `search_workers` gives the worker pool to
/// double oracle alone, so this rate does not scale with the pool.
const BELIEF_TURNS_FOR_EACH_SECOND: u64 = 1_874;

/// The seconds that one answer of a live battle can take.
///
/// This is the target of the whole preset table: one doubles turn, under fog of
/// war, answered inside this many seconds.
const LIVE_TURN_SECONDS: u64 = 30;

/// Resolves the fastest answer. It is the preset for a doubles turn that must
/// answer in a few seconds.
///
/// One damage roll reads the average damage of every attack. It cannot separate
/// a roll that faints a target from a roll that does not, so an exact search
/// here ranks actions without seeing a damage range. A belief search still reads
/// every roll, because a roll does not multiply its work.
pub const FAST_PRESET: PresetLimits = PresetLimits {
    simulation_turn_budget: budget_for(14_482),
    sampled_simulation_turn_budget: sampled_budget_for(BELIEF_TURNS_FOR_EACH_SECOND, 5),
    // The rest of this row matches `BALANCED_PRESET`. Only the clock is shorter.
    depth: PRESET_DEPTH,
    sampled_depth: SAMPLED_PRESET_DEPTH,
    exact_damage_rolls: 1,
    sampled_damage_rolls: 16,
    particles: 16,
};

/// The default preset. It answers a doubles turn inside [`LIVE_TURN_SECONDS`].
///
/// Two exact rolls separate a high roll from a low one, which is the smallest
/// set that tells a faint apart from a survival. The measured doubles solve is
/// 1.71 seconds, so the slowest pairing still fits the live budget.
pub const BALANCED_PRESET: PresetLimits = PresetLimits {
    simulation_turn_budget: budget_for(33_226),
    sampled_simulation_turn_budget: sampled_budget_for(
        BELIEF_TURNS_FOR_EACH_SECOND,
        LIVE_TURN_SECONDS,
    ),
    depth: PRESET_DEPTH,
    sampled_depth: SAMPLED_PRESET_DEPTH,
    exact_damage_rolls: 2,
    sampled_damage_rolls: 16,
    particles: 24,
};

/// The widest preset. Use it for analysis and not for a live battle.
///
/// Three exact rolls cost 7.17 seconds on the measured pairing, so a costlier
/// pairing can pass [`LIVE_TURN_SECONDS`]. The belief search takes four times
/// the live budget for the same reason.
///
/// The measured value moved by 0.005 between one roll and sixteen, and the
/// equilibrium support held five actions at every roll count. Read
/// `benches/RESULTS.md` before you choose this preset for accuracy.
pub const COMPETITIVE_PRESET: PresetLimits = PresetLimits {
    simulation_turn_budget: budget_for(105_432),
    sampled_simulation_turn_budget: sampled_budget_for(
        BELIEF_TURNS_FOR_EACH_SECOND,
        LIVE_TURN_SECONDS * 4,
    ),
    depth: PRESET_DEPTH,
    sampled_depth: SAMPLED_PRESET_DEPTH,
    exact_damage_rolls: 3,
    sampled_damage_rolls: 16,
    particles: 48,
};

/// The limits that a request without a preset name takes.
pub const DEFAULT_PRESET: PresetLimits = BALANCED_PRESET;

// No preset may ask for more worlds than a request may. `check_range` rejects a
// request above the cap, and a preset never reaches that check.
const _: () = assert!(FAST_PRESET.particles <= MAX_PARTICLES);
const _: () = assert!(BALANCED_PRESET.particles <= MAX_PARTICLES);
const _: () = assert!(COMPETITIVE_PRESET.particles <= MAX_PARTICLES);

/// The largest exact roll count that a doubles turn can answer in time.
///
/// `benches/RESULTS.md` measured one depth-1 doubles solve at 7.17 seconds for
/// three rolls and 25.32 seconds for four, on the cheapest pairing that
/// `depth2_cost` reports. A costlier pairing at four rolls passes
/// [`LIVE_TURN_SECONDS`], so three is the ceiling.
const MAX_LIVE_EXACT_ROLLS: u8 = 3;

const _: () = assert!(FAST_PRESET.exact_damage_rolls <= MAX_LIVE_EXACT_ROLLS);
const _: () = assert!(BALANCED_PRESET.exact_damage_rolls <= MAX_LIVE_EXACT_ROLLS);
const _: () = assert!(COMPETITIVE_PRESET.exact_damage_rolls <= MAX_LIVE_EXACT_ROLLS);

// A belief search draws one outcome for each turn, so a roll widens that draw
// rather than multiplying the work. Every preset therefore reads all sixteen.
const _: () = assert!(FAST_PRESET.sampled_damage_rolls == 16);
const _: () = assert!(BALANCED_PRESET.sampled_damage_rolls == 16);
const _: () = assert!(COMPETITIVE_PRESET.sampled_damage_rolls == 16);

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

    // Each family takes its own depth. An enumerating search multiplies its
    // tree for each ply, and a belief search adds one sampled step for each.
    let depth = req.depth.unwrap_or_else(|| preset.depth_for_algorithm(algorithm));
    check_range(scope, "depth", depth, 1, 8)?;
    let replacement_depth = match req.replacement_depth {
        Some(value) => {
            check_range(scope, "replacementDepth", value, 1, 8)?;
            Some(value)
        }
        None => None,
    };
    // A roll multiplies the branch set of an enumerating search, and it only
    // widens one draw for a belief search. The two families therefore take
    // different counts.
    let damage_rolls = req
        .damage_rolls
        .unwrap_or_else(|| preset.damage_rolls_for_algorithm(algorithm));
    check_range(scope, "damageRolls", damage_rolls, 1, 16)?;
    let consider_crit = req.consider_crit.unwrap_or(false);
    let particles = match req.particles {
        Some(value) => {
            check_range(scope, "particles", value, 1, MAX_PARTICLES)?;
            value
        }
        None => preset.particles_for_algorithm(algorithm),
    };
    let exact = algorithm.is_exact();
    // Refinement solves `REFINE_BASE_DEPTH` completely and then raises the cells
    // of the support to `depth`. A request at the base depth has nothing to
    // raise, so the flag would only report a ladder that never ran and would
    // turn iterative deepening off for no gain. Reject it rather than accept a
    // flag that does nothing.
    if req.refine == Some(true) && algorithm.supports_refinement() && depth <= REFINE_BASE_DEPTH {
        return Err(format!(
            "{scope}.refine: refinement raises the cells of the support from \
             depth {REFINE_BASE_DEPTH} to a deeper one, so it needs a depth \
             above {REFINE_BASE_DEPTH}. This request asks for depth {depth}"
        ));
    }
    let refine =
        req.refine.unwrap_or(false) && algorithm.supports_refinement() && depth > REFINE_BASE_DEPTH;
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
        // The default algorithm is double oracle, which enumerates a turn.
        assert_eq!(profile.view.depth, BALANCED_PRESET.depth);
        assert_eq!(
            profile.view.damage_rolls,
            BALANCED_PRESET.exact_damage_rolls
        );
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
            // `ismcts` is a belief search, so it takes the belief limits.
            assert_eq!(profile.view.depth, limits.sampled_depth);
            assert_eq!(profile.view.damage_rolls, limits.sampled_damage_rolls);
            assert_eq!(profile.view.particles, Some(limits.particles));
        }
    }

    /// Each family takes the depth that its cost model supports.
    ///
    /// An enumerating search multiplies its tree by the branch count of a turn
    /// for each ply, so every preset holds it at one turn. A belief search adds
    /// one sampled step for each ply, so it can afford lookahead.
    #[test]
    fn each_family_runs_at_its_own_preset_depth() {
        for name in ["fast", "balanced", "competitive", "custom"] {
            for (algorithm, expected) in [
                ("doubleOracle", PRESET_DEPTH),
                ("pimc", PRESET_DEPTH),
                ("mcts", PRESET_DEPTH),
                ("ismcts", SAMPLED_PRESET_DEPTH),
                ("mccfr", SAMPLED_PRESET_DEPTH),
            ] {
                let request = BotProfileRequest {
                    preset: Some(name.to_string()),
                    algorithm: Some(algorithm.to_string()),
                    ..BotProfileRequest::default()
                };
                let profile = resolve("botP2", &request).unwrap();
                assert_eq!(
                    profile.view.depth, expected,
                    "{name} with {algorithm} must run {expected} turn(s)"
                );
            }
        }
    }

    /// A belief search reads every damage roll at every preset.
    ///
    /// Such a search draws one outcome for each turn, so a roll only widens the
    /// set that the draw comes from. Giving up a roll there saves nothing and
    /// costs outcome fidelity.
    /// The resolved budgets must match the numbers the frontend table holds.
    ///
    /// The server resolves a preset name by itself, so a frontend table that
    /// drifts shows one set of limits and runs another. These are the literals
    /// in `frontend/src/components/solver/solverSettings.ts`. Change both
    /// together, or this test fails and names the row.
    #[test]
    fn the_frontend_preset_table_matches_this_one() {
        for (name, limits, exact, sampled) in [
            ("fast", FAST_PRESET, 57_928, 9_370),
            ("balanced", BALANCED_PRESET, 132_904, 56_220),
            ("competitive", COMPETITIVE_PRESET, 421_728, 224_880),
        ] {
            assert_eq!(
                limits.simulation_turn_budget, exact,
                "{name}: solverSettings.ts holds {exact} for simulationTurnBudget"
            );
            assert_eq!(
                limits.sampled_simulation_turn_budget, sampled,
                "{name}: solverSettings.ts holds {sampled} for                  sampledSimulationTurnBudget"
            );
        }
        // `DEFAULT_SOLVER_SETTINGS` in that file is the balanced row.
        assert_eq!(DEFAULT_PRESET.exact_damage_rolls, 2);
        assert_eq!(DEFAULT_PRESET.sampled_damage_rolls, 16);
        assert_eq!(DEFAULT_PRESET.particles, 24);
        assert_eq!(PRESET_DEPTH, 1);
        assert_eq!(SAMPLED_PRESET_DEPTH, 2);
    }

    #[test]
    fn a_belief_preset_reads_every_damage_roll() {
        for (name, limits) in [
            ("fast", FAST_PRESET),
            ("balanced", BALANCED_PRESET),
            ("competitive", COMPETITIVE_PRESET),
        ] {
            // An enumerating search pays for each roll, so it must read fewer.
            // The two ceilings themselves are compile-time assertions beside the
            // preset table.
            assert!(
                limits.exact_damage_rolls < limits.sampled_damage_rolls,
                "{name} must read fewer rolls when it enumerates a turn"
            );
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
            assert!(upper.exact_damage_rolls > lower.exact_damage_rolls);
            assert!(upper.particles > lower.particles);
            assert!(upper.simulation_turn_budget > lower.simulation_turn_budget);
            assert!(
                upper.sampled_simulation_turn_budget > lower.sampled_simulation_turn_budget
            );
        }
        // A belief search reads every roll at every preset, so that count does
        // not rise. `a_belief_preset_reads_every_damage_roll` holds it.
        assert_eq!(
            COMPETITIVE_PRESET.sampled_damage_rolls, 16,
            "the widest preset reads every roll"
        );
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
                    // Refinement raises the cells of the support above
                    // `REFINE_BASE_DEPTH`, so a request at the preset depth has
                    // nothing to raise and `resolve` rejects it. Ask for one
                    // more turn, so the refined branch is still covered here.
                    depth: refine.then_some(REFINE_BASE_DEPTH + 1),
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
