//! Builds a matchup and plays it.
//!
//! Two callers need the same matchup distribution.
//!
//! 1. `bin/train_eval` collects a corpus of positions to label.
//! 2. `benches/eval_calibration` plays whole games. It measures the evaluator
//!    against the results.
//!
//! One definition keeps both on the same distribution. A curve that measured a
//! different distribution than the fit could not accept or reject the fit.
//!
//! # The matchup
//!
//! [`MatchupConfig`] holds the format. Its default is Pokemon Champions
//! doubles: two active Pokemon, four of six brought, no Terastallization, and
//! Mega Evolution on.
//!
//! A roster comes from an archived teamsheet directory or from the usage cache.
//! [`MatchupConfig::teamsheet_mix`] sets the share of each source. A seed picks
//! the source, the rosters, and the team-preview commands.
//!
//! A seed repeats an archived matchup exactly. It does not repeat a cache
//! matchup. [`crate::meta::generate_meta_team`] reads its candidate list from a
//! `HashMap`, and that order changes with the process. Set
//! [`MatchupConfig::teamsheet_mix`] to 1 when two runs must play the same games.
//!
//! # The play
//!
//! [`TurnPolicy`] chooses one joint action for one side.
//! [`play_turn`] resolves one turn, and [`play_game`] plays until the engine
//! returns a winner or the turn cap stops it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::meta::{MetaDex, generate_meta_team, render_teamsheet};
use crate::simulator::{sample_turn_raw_seeded, team_preview_state_from_team_strings};
use crate::solver::JointActionProb;
use crate::solver::actions::{self, Phase};
use crate::solver::eval::{self, EvalContext};
use crate::solver::mcts::{self, MctsConfig, MctsResult, TransitionMode};
use crate::state::battle::{
    BattleCommand, BattleMechanics, BattleState, MatchState, Player, PlayerCommand,
    TeamPreviewCommand,
};
use crate::state::dex_data::{MoveData, PokemonData};
use crate::state::pokemon::parse_team_sheet_str;

/// The format that a generated matchup plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchupConfig {
    /// Active Pokemon per side. Champions doubles uses two.
    pub active_per_side: u8,
    /// Team members that each side brings. Champions doubles brings four.
    pub brought_per_side: u8,
    /// Team members on each roster. Champions uses six.
    pub roster_size: u8,
    /// The battle rules.
    pub mechanics: BattleMechanics,
    /// The share of matchups that draw from the archived teamsheet directory.
    /// The rest draw from the usage cache.
    pub teamsheet_mix: f64,
}

impl Default for MatchupConfig {
    fn default() -> MatchupConfig {
        MatchupConfig {
            active_per_side: 2,
            brought_per_side: 4,
            roster_size: 6,
            // Champions has no Terastallization, and it keeps Mega Evolution.
            mechanics: BattleMechanics {
                tera_enabled: false,
                mega_enabled: true,
            },
            teamsheet_mix: 0.8,
        }
    }
}

/// One roster and the count of Pokemon that parsed out of it.
///
/// The count comes from the parser rather than the file, because a paste can
/// name a Pokemon or a move that this dex does not hold, and the parser drops
/// that block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roster {
    /// The teamsheet text.
    pub text: String,
    /// The Pokemon that parsed out of `text`.
    pub size: usize,
}

/// The archived rosters that a matchup can draw from.
#[derive(Debug, Clone)]
pub struct TeamPool {
    rosters: Vec<Roster>,
    dropped_short: usize,
}

impl TeamPool {
    /// Loads every `.txt` file of `dir` that parses into a usable roster.
    ///
    /// A roster must hold at least `brought` Pokemon. A shorter one cannot fill
    /// a team-preview command, so it never reaches a battle.
    ///
    /// The function prints nothing. Read [`TeamPool::len`] and
    /// [`TeamPool::dropped_short`] to report the load.
    pub fn load(
        dir: &Path,
        brought: u8,
        pokemon_dex: &HashMap<Species, PokemonData>,
        move_dex: &HashMap<PokemonMove, MoveData>,
    ) -> Result<TeamPool, String> {
        let listing =
            std::fs::read_dir(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        let mut paths: Vec<PathBuf> = listing
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
            .collect();
        // A directory listing has no defined order, and the seed must pick the
        // same pair on every run.
        paths.sort();

        let mut rosters = Vec::new();
        let mut dropped_short = 0usize;
        for path in paths {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let parsed = parse_team_sheet_str(&text, pokemon_dex, move_dex, true);
            if parsed.len() < brought as usize {
                dropped_short += 1;
                continue;
            }
            let size = parsed.len();
            rosters.push(Roster { text, size });
        }
        if rosters.is_empty() {
            return Err(format!("{}: no usable teamsheet", dir.display()));
        }
        Ok(TeamPool {
            rosters,
            dropped_short,
        })
    }

    /// The usable rosters.
    pub fn len(&self) -> usize {
        self.rosters.len()
    }

    /// Whether the pool holds no roster.
    /// [`TeamPool::load`] never returns an empty pool.
    pub fn is_empty(&self) -> bool {
        self.rosters.is_empty()
    }

    /// The files that held fewer Pokemon than the format brings.
    pub fn dropped_short(&self) -> usize {
        self.dropped_short
    }

    /// Two rosters, chosen by seed. The pair is distinct when the pool holds
    /// more than one roster, so a matchup is not a mirror of one team.
    pub fn pair(&self, seed: u64) -> (&Roster, &Roster) {
        let count = self.rosters.len();
        let first = (seed % count as u64) as usize;
        let second = if count == 1 {
            first
        } else {
            let offset = 1 + (seed / count as u64) % (count as u64 - 1);
            ((first as u64 + offset) % count as u64) as usize
        };
        (&self.rosters[first], &self.rosters[second])
    }
}

/// Two rosters and the seed of each side's team-preview command.
///
/// The preview seed travels with the roster, so [`Opening::swapped`] gives each
/// roster the same four Pokemon on the other side of the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    pub p1: Roster,
    pub p2: Roster,
    /// The seed of P1's team-preview command.
    pub p1_seed: u64,
    /// The seed of P2's team-preview command.
    pub p2_seed: u64,
}

impl Opening {
    /// The same two rosters, with the sides exchanged.
    ///
    /// A pair of games, one of each orientation, removes team strength from the
    /// aggregate P1 win rate.
    pub fn swapped(&self) -> Opening {
        Opening {
            p1: self.p2.clone(),
            p2: self.p1.clone(),
            p1_seed: self.p2_seed,
            p2_seed: self.p1_seed,
        }
    }
}

/// Draws the two rosters of one matchup.
///
/// The roll is a fixed function of the seed, so a rerun draws from the same
/// source. An archived draw then repeats exactly. A cache draw does not, because
/// [`generate_meta_team`] reads a `HashMap` order that changes with the process.
///
/// Returns `None` when team generation fails.
pub fn draw_opening(
    config: &MatchupConfig,
    meta_dex: &MetaDex,
    pool: Option<&TeamPool>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
    seed: u64,
) -> Option<Opening> {
    let size = config.roster_size as usize;
    let archived = pool.filter(|_| {
        unit_from_bits(seed.wrapping_mul(0x2545_F491_4F6C_DD1D)) < config.teamsheet_mix
    });

    let (p1, p2) = match archived {
        Some(pool) => {
            let (first, second) = pool.pair(seed);
            (first.clone(), second.clone())
        }
        None => {
            let first = generate_meta_team(meta_dex, pokemon_dex, learnset_dex, size, seed).ok()?;
            let second = generate_meta_team(
                meta_dex,
                pokemon_dex,
                learnset_dex,
                size,
                seed ^ 0xA5A5_A5A5,
            )
            .ok()?;
            if first.len() < size || second.len() < size {
                return None;
            }
            (
                Roster {
                    text: render_teamsheet(&first),
                    size: first.len(),
                },
                Roster {
                    text: render_teamsheet(&second),
                    size: second.len(),
                },
            )
        }
    };

    Some(Opening {
        p1,
        p2,
        p1_seed: seed ^ 0xC3C3_C3C3_C3C3_C3C3,
        p2_seed: seed ^ 0x3C3C_3C3C_3C3C_3C3C,
    })
}

/// Resolves the team preview of one opening into a battle position.
/// Returns `None` when the preview turn does not produce a battle.
pub fn opening_match(
    config: &MatchupConfig,
    opening: &Opening,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    seed: u64,
) -> Option<MatchState> {
    let mut preview = team_preview_state_from_team_strings(
        &opening.p1.text,
        &opening.p2.text,
        pokemon_dex,
        move_dex,
        config.active_per_side,
        config.brought_per_side,
        true,
    );
    // The builder applies `BattleMechanics::default()`, which enables
    // Terastallization. Champions has none, so the caller sets the rules.
    preview.mechanics = config.mechanics;

    // Each side indexes its own roster, and an archived roster can be shorter
    // than `roster_size`. Passing that figure here would name a Pokemon that
    // the sheet does not hold.
    let p1_picks = random_preview_command(
        opening.p1.size,
        config.active_per_side,
        config.brought_per_side,
        opening.p1_seed,
    );
    let p2_picks = random_preview_command(
        opening.p2.size,
        config.active_per_side,
        config.brought_per_side,
        opening.p2_seed,
    );
    let (state, _, _) = sample_turn_raw_seeded(
        seed,
        &MatchState::TeamPreviewState(preview),
        &PlayerCommand::TeamPreview(p1_picks),
        &PlayerCommand::TeamPreview(p2_picks),
        move_dex,
        pokemon_dex,
        false,
        1,
        None,
    );
    matches!(state, MatchState::BattleState(_)).then_some(state)
}

/// Builds one battle position from two generated teams.
/// Returns `None` when generation or the preview turn fails.
pub fn start_match(
    config: &MatchupConfig,
    meta_dex: &MetaDex,
    pool: Option<&TeamPool>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    learnset_dex: &HashMap<Species, HashSet<PokemonMove>>,
    seed: u64,
) -> Option<MatchState> {
    let opening = draw_opening(config, meta_dex, pool, pokemon_dex, learnset_dex, seed)?;
    opening_match(config, &opening, pokemon_dex, move_dex, seed)
}

/// Selects one legal team-preview command from a full roster.
pub fn random_preview_command(
    team_len: usize,
    active_per_side: u8,
    brought_per_side: u8,
    seed: u64,
) -> TeamPreviewCommand {
    let brought = (brought_per_side as usize).min(team_len);
    let active = (active_per_side as usize).min(brought);
    let mut indices: Vec<usize> = (0..team_len).collect();
    indices.shuffle(&mut StdRng::seed_from_u64(seed));
    indices.truncate(brought);
    TeamPreviewCommand {
        active_indices: indices[..active].to_vec(),
        back_indices: indices[active..].to_vec(),
    }
}

/// The weight vector that a [`TurnPolicy::Policy`] play reads.
///
/// The choice decides whether two runs of a caller play the same games. A
/// measurement that compares two weight sets needs [`PolicyWeights::Hand`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyWeights {
    /// [`eval::HAND_POLICY_WEIGHTS`]. The crate holds these as a constant, so
    /// `bin/train_eval` never moves them. Two runs then play the same games.
    Hand,
    /// [`eval::fitted_policy_weights`], which reads `weights/policy_v1.json`.
    /// `bin/train_eval` writes that file, so a training run changes the joint
    /// actions that this option draws.
    Fitted,
}

impl PolicyWeights {
    /// The weight vector itself.
    pub fn values(self) -> &'static eval::PolicyFeatures {
        match self {
            PolicyWeights::Hand => &eval::HAND_POLICY_WEIGHTS,
            PolicyWeights::Fitted => eval::fitted_policy_weights(),
        }
    }

    /// The report label of this weight vector.
    pub fn label(self) -> &'static str {
        match self {
            PolicyWeights::Hand => "hand",
            PolicyWeights::Fitted => "fitted",
        }
    }
}

/// How one side chooses its joint action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TurnPolicy {
    /// One uniform draw over the legal joint actions.
    /// The training corpus uses this policy.
    Random,
    /// A softmax over [`eval::policy_scores`].
    ///
    /// The policy costs no turn simulation, so a whole sweep of games is cheap.
    /// `temperature` divides each score before the softmax.
    /// A small value approaches the highest-scoring action.
    /// A large value approaches a uniform draw.
    ///
    /// `weights` decides whether a training run changes the games. Read
    /// [`PolicyWeights`].
    Policy {
        temperature: f64,
        weights: PolicyWeights,
    },
    /// [`mcts::search`] with a sampled transition.
    ///
    /// This policy costs one turn simulation for each iteration and each ply,
    /// so a sweep of games costs hours instead of seconds.
    ///
    /// One search answers both sides. [`play_turn`] runs it once when the two
    /// sides carry the same settings, so that quoted cost is the whole cost.
    Search { iterations: u32, depth: u8 },
}

impl TurnPolicy {
    /// The report label of this policy.
    pub fn label(&self) -> String {
        match self {
            TurnPolicy::Random => "random".to_string(),
            TurnPolicy::Policy {
                temperature,
                weights,
            } => format!(
                "policy (temperature {temperature}, {} weights)",
                weights.label()
            ),
            TurnPolicy::Search { iterations, depth } => {
                format!("search ({iterations} iterations, depth {depth})")
            }
        }
    }
}

/// Maps the high bits of `bits` onto a value from 0 through 1.
///
/// The caller must supply well-spread bits. [`unit_draw`] mixes first.
fn unit_from_bits(bits: u64) -> f64 {
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// Spreads a seed over the whole 64-bit range.
/// This is the finalizer of SplitMix64.
fn mix(seed: u64) -> u64 {
    let mut value = seed ^ 0x9E37_79B9_7F4A_7C15;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// Maps a seed onto a draw from 0 through 1.
///
/// The seed passes through [`mix`] first.
/// A caller derives a per-side seed by a shift, and a shifted seed carries zeros
/// in its high bits. [`unit_from_bits`] reads those bits.
/// An unmixed shifted seed would therefore give that side a draw near zero on
/// every turn. The side would always take the first action of its list.
fn unit_draw(seed: u64) -> f64 {
    unit_from_bits(mix(seed))
}

/// The index that `draw` selects from `probabilities`.
///
/// The last nonzero entry absorbs a rounding shortfall, so a draw just under 1
/// always finds an entry.
fn pick_index(probabilities: &[f64], draw: f64) -> usize {
    let mut running = 0.0;
    let mut last = 0usize;
    for (index, probability) in probabilities.iter().enumerate() {
        if *probability > 0.0 {
            last = index;
        }
        running += probability;
        if draw < running {
            return index;
        }
    }
    last
}

/// The search settings of a [`TurnPolicy::Search`] play.
fn search_config(iterations: u32, depth: u8) -> MctsConfig {
    MctsConfig {
        iterations,
        depth,
        // A generative transition draws one successor inside turn resolution,
        // so a node costs one trajectory instead of one outcome distribution.
        transition: TransitionMode::Generative { batch: 1 },
        damage_rolls: eval::EVAL_DAMAGE_ROLLS,
        ..MctsConfig::default()
    }
}

/// The index inside `actions` of the joint action that `seed` draws from
/// `strategy`.
///
/// Returns `None` when the strategy is empty, and when it names an action that
/// `actions` does not hold. A search can cap or reorder its own action set, so
/// the caller must handle a miss instead of assuming a position.
fn pick_from_strategy(
    strategy: &[JointActionProb],
    actions: &[Vec<BattleCommand>],
    seed: u64,
) -> Option<usize> {
    if strategy.is_empty() {
        return None;
    }
    let probabilities: Vec<f64> = strategy.iter().map(|entry| entry.probability).collect();
    let chosen = &strategy[pick_index(&probabilities, unit_draw(seed))].commands;
    actions.iter().position(|action| action == chosen)
}

/// Chooses one joint action index for `player`.
///
/// `shared` holds the one search that both sides of this turn read. It is
/// `Some` only when both sides play [`TurnPolicy::Search`] with the same
/// settings. The search then costs one run for the turn instead of two.
#[allow(clippy::too_many_arguments)]
fn choose_action(
    policy: TurnPolicy,
    state: &MatchState,
    player: Player,
    actions: &[Vec<BattleCommand>],
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    seed: u64,
    shared: Option<&MctsResult>,
) -> usize {
    if actions.len() <= 1 {
        return 0;
    }
    let MatchState::BattleState(battle) = state else {
        return 0;
    };
    match policy {
        // The training corpus depends on this exact index, so it stays here.
        TurnPolicy::Random => (seed % actions.len() as u64) as usize,
        TurnPolicy::Policy {
            temperature,
            weights,
        } => {
            let ctx = EvalContext::new(pokemon_dex, move_dex);
            let weights = weights.values();
            let scale = if temperature.is_finite() && temperature > 0.0 {
                temperature
            } else {
                1.0
            };
            let raw: Vec<f64> = actions
                .iter()
                .map(|action| eval::policy_score(battle, player, action, &ctx, weights) / scale)
                .collect();
            let probabilities = eval::softmax(&raw);
            pick_index(&probabilities, unit_draw(seed))
        }
        TurnPolicy::Search { iterations, depth } => {
            let uniform = (seed % actions.len() as u64) as usize;
            let own;
            let result = match shared {
                Some(result) => result,
                None => {
                    let config = search_config(iterations, depth);
                    let Ok(searched) = mcts::search(seed, state, pokemon_dex, move_dex, &config)
                    else {
                        return uniform;
                    };
                    own = searched;
                    &own
                }
            };
            let strategy = match player {
                Player::P1 => &result.p1_strategy,
                Player::P2 => &result.p2_strategy,
            };
            pick_from_strategy(strategy, actions, seed).unwrap_or(uniform)
        }
    }
}

/// Plays one turn with one joint action for each player.
///
/// Returns `None` when the position is not a battle, or when either side has no
/// legal joint action.
pub fn play_turn(
    state: &MatchState,
    p1_policy: TurnPolicy,
    p2_policy: TurnPolicy,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    seed: u64,
) -> Option<MatchState> {
    let MatchState::BattleState(battle) = state else {
        return None;
    };
    let phase = actions::phase_of(state);
    let p1 = actions::joint_actions(
        battle,
        Player::P1,
        phase,
        move_dex,
        pokemon_dex,
        None,
        false,
    );
    let p2 = actions::joint_actions(
        battle,
        Player::P2,
        phase,
        move_dex,
        pokemon_dex,
        None,
        false,
    );
    if p1.actions.is_empty() || p2.actions.is_empty() {
        return None;
    }

    // One `mcts::search` returns both root strategies. A second search would
    // double the cost of the search policy and change no answer. Each side still
    // draws from its own marginal, and each side still reads its own seed.
    let shared = match (p1_policy, p2_policy) {
        (
            TurnPolicy::Search { iterations, depth },
            TurnPolicy::Search {
                iterations: p2_iterations,
                depth: p2_depth,
            },
        ) if iterations == p2_iterations && depth == p2_depth => mcts::search(
            seed,
            state,
            pokemon_dex,
            move_dex,
            &search_config(iterations, depth),
        )
        .ok(),
        _ => None,
    };

    let p1_pick = choose_action(
        p1_policy,
        state,
        Player::P1,
        &p1.actions,
        pokemon_dex,
        move_dex,
        seed,
        shared.as_ref(),
    );
    // The two sides read different bits of the seed, so they do not pick the
    // same index out of two equal-length action lists.
    let p2_pick = choose_action(
        p2_policy,
        state,
        Player::P2,
        &p2.actions,
        pokemon_dex,
        move_dex,
        seed >> 17,
        shared.as_ref(),
    );
    let (next, _, _) = sample_turn_raw_seeded(
        seed,
        state,
        &PlayerCommand::Battle(p1.actions[p1_pick].clone()),
        &PlayerCommand::Battle(p2.actions[p2_pick].clone()),
        move_dex,
        pokemon_dex,
        false,
        1,
        None,
    );
    Some(next)
}

/// Plays one turn with a random legal joint action for each player.
pub fn play_random_turn(
    state: &MatchState,
    move_dex: &HashMap<PokemonMove, MoveData>,
    pokemon_dex: &HashMap<Species, PokemonData>,
    seed: u64,
) -> Option<MatchState> {
    play_turn(
        state,
        TurnPolicy::Random,
        TurnPolicy::Random,
        move_dex,
        pokemon_dex,
        seed,
    )
}

/// How a whole game plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameConfig {
    /// P1's policy.
    pub p1: TurnPolicy,
    /// P2's policy.
    pub p2: TurnPolicy,
    /// Steps that the game may take. A replacement step and a self-switch step
    /// each consume one.
    /// A battle that is still running at the cap has no result.
    pub turn_cap: usize,
}

impl Default for GameConfig {
    fn default() -> GameConfig {
        GameConfig {
            p1: TurnPolicy::Policy {
                temperature: 1.0,
                weights: PolicyWeights::Fitted,
            },
            p2: TurnPolicy::Policy {
                temperature: 1.0,
                weights: PolicyWeights::Fitted,
            },
            turn_cap: 150,
        }
    }
}

/// What one played game produced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameResult {
    /// The winner. A battle that is still running at the turn cap has no winner.
    /// A position that offers no legal joint action has no winner. The game then
    /// has no result.
    pub winner: Option<Player>,
    /// Steps that the game resolved.
    /// A replacement step and a self-switch step each count one, so this figure
    /// is at least the count of ordinary turns.
    pub turns: usize,
}

/// Plays one matchup to its end.
///
/// `on_turn` runs before each ordinary turn, with the position that both sides
/// are about to choose from. A replacement phase and a self-switch phase do not
/// call it, because those are the tail of a turn that already ran.
///
/// The game stops at the first of three events: the engine returns a winner,
/// `config.turn_cap` steps resolve, or a position offers no legal joint action.
///
/// A stuck position returns no winner. The cap returns no winner as well. The
/// one exception is a last permitted step that ended the battle. That game
/// finished inside the budget, so it keeps its winner.
///
/// A caller must drop the positions of a game that has no winner.
///
/// `config.turn_cap` counts steps and not ordinary turns. A replacement step and
/// a self-switch step each consume one.
pub fn play_game(
    start: &MatchState,
    config: &GameConfig,
    pokemon_dex: &HashMap<Species, PokemonData>,
    move_dex: &HashMap<PokemonMove, MoveData>,
    seed: u64,
    on_turn: &mut dyn FnMut(&BattleState),
) -> GameResult {
    let mut state = start.clone();
    for turn in 0..config.turn_cap {
        match &state {
            MatchState::GameOverState { winner, .. } => {
                return GameResult {
                    winner: Some(*winner),
                    turns: turn,
                };
            }
            MatchState::TeamPreviewState(_) => {
                return GameResult {
                    winner: None,
                    turns: turn,
                };
            }
            MatchState::BattleState(battle) => {
                if matches!(actions::phase_of(&state), Phase::Normal) {
                    on_turn(battle);
                }
            }
        }

        let turn_seed = seed.wrapping_add(turn as u64 * 0x1000_0000_0000_0001);
        let Some(next) = play_turn(
            &state,
            config.p1,
            config.p2,
            move_dex,
            pokemon_dex,
            turn_seed,
        ) else {
            return GameResult {
                winner: None,
                turns: turn,
            };
        };
        state = next;
    }

    // The last permitted step can itself end the battle. That game finished
    // inside the budget, so it keeps its winner. Only a battle that is still
    // running at the cap has no result.
    if let MatchState::GameOverState { winner, .. } = &state {
        return GameResult {
            winner: Some(*winner),
            turns: config.turn_cap,
        };
    }
    GameResult {
        winner: None,
        turns: config.turn_cap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(text: &str, size: usize) -> Roster {
        Roster {
            text: text.to_string(),
            size,
        }
    }

    #[test]
    fn a_preview_selects_only_the_brought_members_from_the_roster() {
        let command = random_preview_command(6, 2, 4, 17);
        assert_eq!(command.active_indices.len(), 2);
        assert_eq!(command.back_indices.len(), 2);
        let mut selected = command.active_indices;
        selected.extend(command.back_indices);
        selected.sort_unstable();
        selected.dedup();
        assert_eq!(selected.len(), 4);
        assert!(selected.iter().all(|index| *index < 6));
    }

    #[test]
    fn a_preview_of_a_short_roster_stays_inside_it() {
        let command = random_preview_command(3, 2, 4, 21);
        assert_eq!(command.active_indices.len(), 2);
        assert_eq!(command.back_indices.len(), 1);
        assert!(
            command
                .active_indices
                .iter()
                .chain(command.back_indices.iter())
                .all(|index| *index < 3)
        );
    }

    #[test]
    fn a_swapped_opening_pairs_the_two_rosters_in_the_opposite_order() {
        let opening = Opening {
            p1: roster("first", 6),
            p2: roster("second", 5),
            p1_seed: 11,
            p2_seed: 22,
        };
        let swapped = opening.swapped();
        assert_eq!(swapped.p1, opening.p2);
        assert_eq!(swapped.p2, opening.p1);
        // The preview seed travels with the roster, so each side brings the
        // same four Pokemon that it brought in the first game.
        assert_eq!(swapped.p1_seed, opening.p2_seed);
        assert_eq!(swapped.p2_seed, opening.p1_seed);
        assert_eq!(swapped.swapped(), opening);
    }

    #[test]
    fn the_default_matchup_describes_champions_doubles() {
        let config = MatchupConfig::default();
        assert_eq!(config.active_per_side, 2);
        assert_eq!(config.brought_per_side, 4);
        assert_eq!(config.roster_size, 6);
        assert!(!config.mechanics.tera_enabled);
        assert!(config.mechanics.mega_enabled);
    }

    #[test]
    fn a_pool_of_one_roster_pairs_it_with_itself() {
        let pool = TeamPool {
            rosters: vec![roster("only", 6)],
            dropped_short: 2,
        };
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_empty());
        assert_eq!(pool.dropped_short(), 2);
        for seed in 0..8u64 {
            let (first, second) = pool.pair(seed);
            assert_eq!(first, second);
        }
    }

    #[test]
    fn a_pool_of_several_rosters_never_pairs_one_with_itself() {
        let pool = TeamPool {
            rosters: (0..5)
                .map(|index| roster(&format!("t{index}"), 6))
                .collect(),
            dropped_short: 0,
        };
        for seed in 0..200u64 {
            let (first, second) = pool.pair(seed);
            assert_ne!(first, second);
        }
    }

    #[test]
    fn a_pick_finds_the_entry_that_the_draw_lands_in() {
        let probabilities = [0.25, 0.5, 0.25];
        assert_eq!(pick_index(&probabilities, 0.0), 0);
        assert_eq!(pick_index(&probabilities, 0.24), 0);
        assert_eq!(pick_index(&probabilities, 0.25), 1);
        assert_eq!(pick_index(&probabilities, 0.74), 1);
        assert_eq!(pick_index(&probabilities, 0.75), 2);
        // A rounding shortfall must still land on a nonzero entry.
        assert_eq!(pick_index(&[0.5, 0.5, 0.0], 0.999_999_999), 1);
    }

    #[test]
    fn a_unit_draw_stays_inside_its_range() {
        for seed in 0..1000u64 {
            let draw = unit_draw(seed);
            assert!((0.0..1.0).contains(&draw), "seed {seed} gave {draw}");
            let raw = unit_from_bits(seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
            assert!((0.0..1.0).contains(&raw), "seed {seed} gave {raw}");
        }
    }

    #[test]
    fn a_shifted_seed_still_covers_the_whole_draw_range() {
        // P2 reads `seed >> 17`, which has 17 zero bits at the top. Without the
        // mixing step every such draw would sit under 2^-17, and P2 would always
        // take the first action of its list.
        let mean: f64 = (0..4000u64)
            .map(|seed| unit_draw(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 17))
            .sum::<f64>()
            / 4000.0;
        assert!((mean - 0.5).abs() < 0.02, "shifted seeds averaged {mean}");
    }

    /// The seed that [`play_game`] hands step `step`.
    fn step_seed(game_seed: u64, step: usize) -> u64 {
        game_seed.wrapping_add(step as u64 * 0x1000_0000_0000_0001)
    }

    fn test_pokemon_dex() -> &'static HashMap<Species, PokemonData> {
        crate::tests::simuilator_test_helpers::pokemon_dex()
    }

    fn test_move_dex() -> &'static HashMap<PokemonMove, MoveData> {
        crate::tests::simuilator_test_helpers::move_dex()
    }

    /// A doubles position with a bench, so each side offers many joint actions.
    fn doubles_position() -> MatchState {
        use crate::data::ability::Ability;
        use crate::state::pokemon::{Nature, PokemonState, build_pokemon_state};
        use crate::tests::simuilator_test_helpers::battle_state_from_lists;

        fn mon(species: Species, moves: &[PokemonMove]) -> PokemonState {
            let mut slots: [Option<PokemonMove>; 4] = [None, None, None, None];
            for (slot, name) in slots.iter_mut().zip(moves) {
                *slot = Some(name.clone());
            }
            build_pokemon_state(
                species,
                test_pokemon_dex(),
                test_move_dex(),
                Some(50),
                Some(slots),
                None,
                Some(Ability::None),
                Some(Nature::Serious),
                None,
                None,
                Some([0; 6]),
                Some([31; 6]),
                false,
            )
        }

        MatchState::BattleState(battle_state_from_lists(
            vec![
                mon(
                    Species::Pikachu,
                    &[PokemonMove::Thunderbolt, PokemonMove::QuickAttack],
                ),
                mon(
                    Species::Snorlax,
                    &[PokemonMove::BodySlam, PokemonMove::Crunch],
                ),
            ],
            vec![mon(Species::Gyarados, &[PokemonMove::Waterfall])],
            vec![
                mon(
                    Species::Garchomp,
                    &[PokemonMove::DragonClaw, PokemonMove::Earthquake],
                ),
                mon(
                    Species::Gengar,
                    &[PokemonMove::ShadowBall, PokemonMove::SludgeBomb],
                ),
            ],
            vec![mon(Species::Aerodactyl, &[PokemonMove::RockSlide])],
        ))
    }

    #[test]
    fn p2_spreads_its_policy_draw_over_the_seeds_that_a_game_hands_it() {
        // `play_turn` gives P2 `seed >> 17`. That shift leaves 17 zero bits at
        // the top, so an unmixed draw always sat under 2 to the power of -17 and
        // P2 always took the first action of its list. `unit_draw` mixes first.
        // This test pins the whole chain and not `unit_draw` alone.
        let state = doubles_position();
        let MatchState::BattleState(battle) = &state else {
            unreachable!("the helper builds a battle");
        };
        let p2 = actions::joint_actions(
            battle,
            Player::P2,
            actions::phase_of(&state),
            test_move_dex(),
            test_pokemon_dex(),
            None,
            false,
        );
        assert!(
            p2.actions.len() > 4,
            "the position must offer P2 a real choice, not {}",
            p2.actions.len()
        );

        let policy = TurnPolicy::Policy {
            temperature: 1.0,
            weights: PolicyWeights::Fitted,
        };
        let draws = 200usize * 8;
        let mut first_action = 0usize;
        let mut distinct = HashSet::new();
        for game in 0..200usize {
            let game_seed = (game as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            for step in 0..8usize {
                let picked = choose_action(
                    policy,
                    &state,
                    Player::P2,
                    &p2.actions,
                    test_pokemon_dex(),
                    test_move_dex(),
                    step_seed(game_seed, step) >> 17,
                    None,
                );
                distinct.insert(picked);
                if picked == 0 {
                    first_action += 1;
                }
            }
        }
        assert!(
            distinct.len() > 1,
            "P2 only ever picked {distinct:?} across {draws} draws"
        );
        // The old defect pinned this at every draw.
        assert!(
            first_action < draws,
            "P2 took the first action on every one of {draws} draws"
        );
    }

    #[test]
    fn a_game_that_ends_on_the_last_permitted_step_keeps_its_winner() {
        let start = doubles_position();
        let config = GameConfig {
            p1: TurnPolicy::Random,
            p2: TurnPolicy::Random,
            turn_cap: 300,
        };
        let seed = 7u64;
        let full = play_game(
            &start,
            &config,
            test_pokemon_dex(),
            test_move_dex(),
            seed,
            &mut |_| {},
        );
        let winner = full
            .winner
            .expect("a random doubles game settles well inside 300 steps");
        assert!(full.turns > 1);

        // The cap now equals the length of the finished game, so the last
        // permitted step is the step that ended the battle. That game finished
        // inside the budget and must keep its winner.
        let exact = play_game(
            &start,
            &GameConfig {
                turn_cap: full.turns,
                ..config
            },
            test_pokemon_dex(),
            test_move_dex(),
            seed,
            &mut |_| {},
        );
        assert_eq!(exact.winner, Some(winner));
        assert_eq!(exact.turns, full.turns);

        // One step short, and the battle is still running at the cap.
        let short = play_game(
            &start,
            &GameConfig {
                turn_cap: full.turns - 1,
                ..config
            },
            test_pokemon_dex(),
            test_move_dex(),
            seed,
            &mut |_| {},
        );
        assert_eq!(short.winner, None);
        assert_eq!(short.turns, full.turns - 1);
    }

    #[test]
    fn a_game_records_an_ordinary_turn_and_never_a_replacement_step() {
        let start = doubles_position();
        let mut seen = 0usize;
        let result = play_game(
            &start,
            &GameConfig {
                p1: TurnPolicy::Random,
                p2: TurnPolicy::Random,
                turn_cap: 300,
            },
            test_pokemon_dex(),
            test_move_dex(),
            11,
            &mut |battle| {
                // A replacement step and a self-switch step are the tail of a
                // turn that already ran, so neither may reach the callback.
                assert!(battle.self_switch_pending.is_none());
                assert!(!(battle.turn_started && battle.turn_ended));
                seen += 1;
            },
        );
        assert!(result.winner.is_some());
        assert!(seen > 0);
        // The step count also holds the replacement steps, so it cannot be
        // smaller than the count of ordinary turns.
        assert!(seen <= result.turns);
    }

    #[test]
    fn a_policy_names_itself() {
        assert_eq!(TurnPolicy::Random.label(), "random");
        assert!(
            TurnPolicy::Search {
                iterations: 64,
                depth: 2
            }
            .label()
            .contains("64 iterations")
        );
        // The report must name the weight source. The two sources answer two
        // different questions, and only one of them holds the games still.
        assert!(
            TurnPolicy::Policy {
                temperature: 1.0,
                weights: PolicyWeights::Hand,
            }
            .label()
            .contains("hand")
        );
        assert!(
            TurnPolicy::Policy {
                temperature: 1.0,
                weights: PolicyWeights::Fitted,
            }
            .label()
            .contains("fitted")
        );
    }

    #[test]
    fn the_two_weight_sources_are_two_different_vectors() {
        // A training run rewrites `weights/policy_v1.json` and never touches the
        // constant. `PolicyWeights::Hand` is therefore the source that holds the
        // games still across two runs.
        assert_eq!(
            PolicyWeights::Hand.values(),
            &crate::solver::eval::HAND_POLICY_WEIGHTS
        );
        assert_ne!(PolicyWeights::Fitted.values(), PolicyWeights::Hand.values());
    }
}
