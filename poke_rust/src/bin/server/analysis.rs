//! The private P2 analysis job of a battle session.
//!
//! A session with a resolved P2 profile starts one search when the session
//! begins and one search after each resolved turn. The first position is the
//! team preview, so the bot picks its leads from a solved strategy rather than
//! at random. The search runs on a blocking thread, so it never holds the
//! session lock. Each job carries a generation and a job ID.
//!
//! [`AnalysisState::invalidate`] raises the generation and cancels the running
//! job. [`AnalysisState::accept`] drops a result when its generation or job ID
//! is not current. Thus, a slow job cannot overwrite a newer answer. Both calls
//! keep the newest checkpoint after a failure or a cancellation.
//!
//! Every search reads the cancel flag during the search, so a raised flag stops
//! the work rather than only the start of it. A cancelled search still returns
//! an answer, and that answer covers only the work that finished. State changes
//! drop that partial answer. An explicit finish request keeps it as the current
//! strategy.
//!
//! P1 reads `GET /api/battles/{id}/analysis` during a bot battle. The response
//! includes the current P2 strategy and win estimate. It also includes the
//! exact simulation count.
//!
//! [`draw_p2_command`] draws the command that P2 plays. The turn response
//! reveals that one command after the turn resolves, so both commands are
//! already locked. See [`P2Draw`] for what the reveal carries.
//!
//! [`progress_dto`] renders the strategy of the current position.
//! [`reveal_dto`] renders the strategy that supplied the drawn command.
//! A hotseat battle has no bot profile and sends no bot strategy.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use poke_rust::information::determinize::DeterminizeConfig;
use poke_rust::information::inference::InferenceConfig;
use poke_rust::information::unknowns::UnknownMatchState;
use poke_rust::meta::{MetaDex, MetaFormat};
use poke_rust::solver::preview::{
    OpenListConfig, PreviewChoiceProb, PreviewConfig, PreviewRound,
    solve_open_list_preview_progress_cancellable, solve_team_preview_progress_cancellable,
};
use poke_rust::solver::{self, CancelFlag, JointActionProb};
use poke_rust::state::battle::{
    BattleCommand, MatchState, Player, PlayerCommand, TeamPreviewCommand, TeamPreviewState,
};

use crate::bot::{BotSearchConfig, MAX_SAFE_INTEGER};
use crate::dto::{
    AnalysisCheckpointDto, AnalysisProgressDto, AnalysisReplayDto, P2RevealDto, P2StrategyDto,
    PlayerCommandDto, PreviewChoiceDto, StrategyRowDto,
};
use crate::mapping;
use crate::session::{self, BattleSession, Dexes, MetaDexes};
use crate::tracker_analysis::{preview_battle_config, preview_worlds};

/// The completed answer of one job.
///
/// The four solver result types share these fields, so one checkpoint covers
/// every algorithm.
#[derive(Debug, Clone)]
pub struct AnalysisCheckpoint {
    /// The generation of the position that the search read.
    pub generation: u64,
    pub turn_number: u16,
    /// P2's odds of winning, in `[0, 1]`.
    pub p2_win_odds: f64,
    /// True when the solver completed this depth.
    pub complete: bool,
    /// P2's mixed strategy at the root.
    ///
    /// The analysis endpoint returns it for the current bot position.
    /// [`draw_p2_command`] also draws one action from it.
    pub p2_strategy: P2Strategy,
    /// The depth that the search reached.
    /// A sampling search reports its configured depth.
    pub depth_reached: u8,
    /// Completed `simulate_turn` calls.
    ///
    /// No endpoint returns this. A depth-one exact search spends one call per
    /// matrix cell, so the figure is about `|A1| * |A2|`, and P1 already holds
    /// `|A1|`. That divides out P2's joint-action count, which is the same
    /// figure [`warning_line`] scrubs out of
    /// [`solver::SolveWarning::ActionsTruncated`]. The later reveal item reads
    /// it.
    #[allow(dead_code)]
    pub turns_simulated: u64,
    /// Decision nodes for an exact search, tree nodes for a sampling search.
    /// Private for the same reason as `turns_simulated`.
    #[allow(dead_code)]
    pub nodes: u64,
    pub elapsed: Duration,
    /// The seed of this search, which makes the result reproducible.
    pub seed: u64,
    /// The data that repeats this search.
    pub replay: AnalysisReplay,
    /// True when the strategy respects the session's fog of war.
    pub strategy_is_playable: bool,
    /// Every reason that the answer is approximate.
    pub warnings: Vec<String>,
}

/// P2's mixed strategy, and the position that it belongs to.
///
/// A battle position and a team preview take different command types, so one
/// list cannot serve both. The variant names the position that the search read.
/// [`draw_p2_command`] reads only the arm of the position that it draws for, so
/// a checkpoint of the other arm sends the draw to its uniform case.
#[derive(Debug, Clone)]
pub enum P2Strategy {
    /// One joint action for each battle slot.
    Battle(Vec<JointActionProb>),
    /// One bring-and-lead choice.
    Preview(Vec<PreviewChoiceProb>),
}

impl P2Strategy {
    /// True when the strategy holds no action.
    ///
    /// [`complete_line`] reports this one bit, and no count. See the note there.
    fn is_empty(&self) -> bool {
        match self {
            P2Strategy::Battle(rows) => rows.is_empty(),
            P2Strategy::Preview(rows) => rows.is_empty(),
        }
    }
}

/// The data that repeats one analysis search.
///
/// Every field is a position identifier, a seed, or a public profile setting.
/// An operator with the session history can run the same search again.
#[derive(Debug, Clone, Default)]
pub struct AnalysisReplay {
    /// The generation and turn identify the position in the session history.
    pub generation: u64,
    pub turn_number: u16,
    /// The seed of the search.
    pub search_seed: u64,
    pub algorithm: String,
    pub simulation_turn_budget: u64,
    pub depth: u8,
    /// Absent when a forced decision uses the remaining turn budget.
    pub replacement_depth: Option<u8>,
    pub workers: u8,
    pub particles: Option<usize>,
    pub damage_rolls: u8,
    pub consider_crit: bool,
    pub turns_simulated: u64,
}

/// The job that is running now.
#[derive(Debug)]
struct RunningJob {
    id: u64,
    generation: u64,
    started: Instant,
    /// Stops the job before its search starts, and during the search.
    ///
    /// Every search reads this flag at its own check points. The ticket check in
    /// [`AnalysisState::accept`] covers the final race.
    cancel: CancelFlag,
    /// True when the player asked the job to return its current strategy.
    finish_requested: Arc<AtomicBool>,
}

/// Identifies one analysis job and carries its cancel flag.
#[derive(Debug)]
struct JobTicket {
    id: u64,
    generation: u64,
    cancel: CancelFlag,
    finish_requested: Arc<AtomicBool>,
}

impl JobTicket {
    fn should_publish_cancelled_result(&self) -> bool {
        self.finish_requested.load(Ordering::Acquire)
    }
}

/// The analysis record of one session.
#[derive(Debug, Default)]
pub struct AnalysisState {
    /// The generation of the current position.
    /// Every state change raises it by one.
    generation: u64,
    /// The ID for the next job in this session.
    next_job_id: u64,
    running: Option<RunningJob>,
    /// The newest strategy checkpoint.
    /// A failure and a cancellation both keep it.
    checkpoint: Option<AnalysisCheckpoint>,
    /// Why the last job produced no checkpoint.
    last_error: Option<String>,
}

impl AnalysisState {
    /// Raises the generation and cancels the running job.
    ///
    /// Keeps the checkpoint. The client reads the last answer until a newer
    /// checkpoint arrives. The `stale` flag marks the old answer.
    pub fn invalidate(&mut self) {
        self.generation += 1;
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
    }

    /// Records the job that is starting and returns its ticket.
    /// Cancels an earlier job of the same generation, so one job runs at a time.
    #[cfg(test)]
    fn start(&mut self) -> JobTicket {
        self.start_with_budget(0)
    }

    /// Starts a job with one simulation-turn budget.
    fn start_with_budget(&mut self, budget: u64) -> JobTicket {
        if let Some(job) = self.running.take() {
            job.cancel.cancel();
        }
        let id = self.next_job_id;
        self.next_job_id = self.next_job_id.wrapping_add(1);
        let cancel = if budget == 0 {
            CancelFlag::new()
        } else {
            CancelFlag::with_simulation_turn_budget(budget)
        };
        let finish_requested = Arc::new(AtomicBool::new(false));
        self.running = Some(RunningJob {
            id,
            generation: self.generation,
            started: Instant::now(),
            cancel: cancel.clone(),
            finish_requested: Arc::clone(&finish_requested),
        });
        self.last_error = None;
        JobTicket {
            id,
            generation: self.generation,
            cancel,
            finish_requested,
        }
    }

    /// Stops the current search and keeps the strategy that it has found.
    pub fn finish_now(&mut self) -> Result<(), String> {
        if let Some(job) = &self.running {
            job.finish_requested.store(true, Ordering::Release);
            job.cancel.cancel();
            return Ok(());
        }
        if self.current_checkpoint().is_some() {
            return Ok(());
        }
        Err("analysis: no current search or strategy is available".to_string())
    }

    /// Stores the result of one job.
    ///
    /// Drops a result from an old or replaced job. A failure keeps the last
    /// checkpoint.
    ///
    /// Returns the console line of the exit that ran. The caller writes that
    /// line after it releases the session lock. A job that leaves no checkpoint
    /// for the current generation makes the client report "no answer", and the
    /// four lines name which exit ran.
    fn accept(&mut self, job: &JobTicket, outcome: Result<AnalysisCheckpoint, String>) -> String {
        if job.generation != self.generation {
            return format!(
                "analysis job {}: the result was dropped, \
                 because the position moved from generation {} to generation {}",
                job.id, job.generation, self.generation
            );
        }
        if !self
            .running
            .as_ref()
            .is_some_and(|running| running.id == job.id && running.generation == job.generation)
        {
            return format!(
                "analysis job {} (generation {}): the result was dropped, \
                 because another job replaced it",
                job.id, job.generation
            );
        }
        self.running = None;
        match outcome {
            Ok(checkpoint) => {
                let line = complete_line(job, &checkpoint);
                self.store_newest(checkpoint);
                self.last_error = None;
                line
            }
            Err(message) => {
                let line = format!(
                    "analysis job {} (generation {}): no checkpoint: {message}",
                    job.id, job.generation
                );
                self.last_error = Some(message);
                line
            }
        }
    }

    /// Stores a solver checkpoint while the job continues.
    fn publish(&mut self, job: &JobTicket, checkpoint: AnalysisCheckpoint) {
        if job.generation != self.generation {
            return;
        }
        if !self
            .running
            .as_ref()
            .is_some_and(|running| running.id == job.id && running.generation == job.generation)
        {
            return;
        }
        self.store_newest(checkpoint);
    }

    fn store_newest(&mut self, checkpoint: AnalysisCheckpoint) {
        let replace = self.checkpoint.as_ref().is_none_or(|current| {
            checkpoint.generation != current.generation
                || checkpoint.depth_reached > current.depth_reached
                || (checkpoint.depth_reached == current.depth_reached
                    && checkpoint.turns_simulated >= current.turns_simulated)
        });
        if replace {
            self.checkpoint = Some(checkpoint);
        }
    }

    /// The private progress of this session.
    ///
    /// The response holds no P2 action, no P2 strategy, and no P2 win odds. Only
    /// the cost of the search and the generation of each answer appear.
    pub fn progress(&self) -> AnalysisProgressDto {
        let phase = if self.running.is_some() {
            "running"
        } else if self.last_error.is_some() {
            "failed"
        } else if self.checkpoint.is_some() {
            "complete"
        } else {
            "idle"
        };
        AnalysisProgressDto {
            generation: self.generation,
            phase: phase.to_string(),
            running_ms: self
                .running
                .as_ref()
                .map(|job| job.started.elapsed().as_millis() as u64),
            turns_simulated: self
                .running
                .as_ref()
                .map(|job| job.cancel.simulation_turns()),
            simulation_turn_budget: self
                .running
                .as_ref()
                .and_then(|job| job.cancel.simulation_turn_budget()),
            checkpoint: self
                .checkpoint
                .as_ref()
                .map(|c| checkpoint_dto(c, self.generation)),
            error: self.last_error.clone(),
        }
    }

    /// The generation of the current position.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The checkpoint of the current position.
    ///
    /// Returns `None` while the newest complete answer belongs to an older
    /// position, because a stale strategy names actions that the current
    /// position can no longer play.
    fn current_checkpoint(&self) -> Option<&AnalysisCheckpoint> {
        self.checkpoint
            .as_ref()
            .filter(|c| c.generation == self.generation)
    }

    /// The last strategy checkpoint, stale or not.
    #[cfg(test)]
    fn checkpoint(&self) -> Option<&AnalysisCheckpoint> {
        self.checkpoint.as_ref()
    }
}

/// The console line of one complete job.
///
/// The line names the cost of the search. It never names the number of rows in
/// `p2_strategy`. That number is P2's joint-action count, which
/// [`AnalysisCheckpoint::turns_simulated`] keeps off every response and which
/// [`warning_line`] scrubs out of `ActionsTruncated`. P1 runs the server in a
/// hotseat battle, so the console must hold no more of P2's plan than the
/// response does.
///
/// An empty strategy explains the "no action with a positive weight" draw line,
/// so the line reports that one fact alone.
fn complete_line(job: &JobTicket, checkpoint: &AnalysisCheckpoint) -> String {
    let strategy = if checkpoint.p2_strategy.is_empty() {
        "no action"
    } else {
        "an action"
    };
    format!(
        "analysis job {} (generation {}): complete, depth {}, {} ms, \
         the strategy holds {strategy}, playable {}",
        job.id,
        job.generation,
        checkpoint.depth_reached,
        checkpoint.elapsed.as_millis(),
        checkpoint.strategy_is_playable
    )
}

/// Builds the public progress row of one checkpoint.
///
/// The row carries the wall-clock cost and the exact simulation count.
///
/// [`progress_dto`] adds the strategy for a bot session.
fn checkpoint_dto(checkpoint: &AnalysisCheckpoint, generation: u64) -> AnalysisCheckpointDto {
    AnalysisCheckpointDto {
        generation: checkpoint.generation,
        stale: checkpoint.generation != generation,
        turn_number: checkpoint.turn_number,
        depth_reached: checkpoint.depth_reached,
        complete: checkpoint.complete,
        p2_win_odds: checkpoint.p2_win_odds,
        elapsed_ms: checkpoint.elapsed.as_millis() as u64,
        turns_simulated: checkpoint.turns_simulated,
        seed: checkpoint.seed,
        warnings: checkpoint.warnings.clone(),
        p2_strategy: None,
    }
}

/// The analysis response of one session.
///
/// [`AnalysisState::progress`] builds the private view, which every session
/// gets. A bot session also gets P2's strategy at the current position.
///
/// Only a checkpoint of the current position carries rows. A stale checkpoint
/// names actions of an older position, and [`strategy_dto`] would render them
/// against a field that no longer holds those Pokemon.
pub fn progress_dto(session: &BattleSession) -> AnalysisProgressDto {
    let mut view = session.analysis.progress();
    if session.bot_p2.is_none() {
        return view;
    }
    let Some(checkpoint) = session.analysis.current_checkpoint() else {
        return view;
    };
    if let Some(row) = view.checkpoint.as_mut() {
        row.p2_strategy = strategy_dto(&session.state, &checkpoint.p2_strategy, None);
    }
    view
}

fn reveals_strategy(session: &BattleSession) -> bool {
    session.bot_p2.is_some()
}

// ── The revealed strategy ────────────────────────────────────────────────────

/// Renders P2's strategy against the position that the search read.
///
/// `drawn` is the index in `strategy` of the row that supplied a drawn command.
/// The rendered block reports where that row landed after the sort.
///
/// Returns `None` when the strategy and the position disagree, because a battle
/// row cannot render against a preview roster and a preview row cannot render
/// against a field. [`draw_p2_command`] applies the same rule.
fn strategy_dto(
    state: &MatchState,
    strategy: &P2Strategy,
    drawn: Option<usize>,
) -> Option<P2StrategyDto> {
    match (state, strategy) {
        (MatchState::BattleState(battle), P2Strategy::Battle(rows)) => {
            let (kept, total) = ranked(rows, |row| row.probability);
            Some(P2StrategyDto {
                position: "battle".to_string(),
                drawn_index: drawn_index(&kept, drawn),
                rows: kept
                    .iter()
                    .map(|(_, action)| StrategyRowDto {
                        commands: action
                            .commands
                            .iter()
                            .enumerate()
                            .map(|(slot_idx, command)| {
                                mapping::command_option(battle, Player::P2, slot_idx, command)
                            })
                            .collect(),
                        preview: None,
                        probability: action.probability,
                    })
                    .collect(),
                total,
            })
        }
        (MatchState::TeamPreviewState(preview), P2Strategy::Preview(rows)) => {
            let (kept, total) = ranked(rows, |row| row.probability);
            Some(P2StrategyDto {
                position: "teamPreview".to_string(),
                drawn_index: drawn_index(&kept, drawn),
                rows: kept
                    .iter()
                    .map(|(_, row)| StrategyRowDto {
                        commands: Vec::new(),
                        preview: Some(PreviewChoiceDto {
                            leads: roster_names(&preview.p2_mons, &row.choice.active_indices),
                            back: roster_names(&preview.p2_mons, &row.choice.back_indices),
                        }),
                        probability: row.probability,
                    })
                    .collect(),
                total,
            })
        }
        _ => None,
    }
}

/// The rows that one response carries, highest rate first.
///
/// Each entry keeps its index in the source strategy, so [`drawn_index`] can
/// name the drawn row after the sort. A row with a rate of zero never plays, so
/// it does not appear and it does not count toward the total. `sort_by` is
/// stable, so a tie keeps the order that the solver returned.
///
/// Returns all positive rows and their count.
fn ranked<T>(rows: &[T], weight: impl Fn(&T) -> f64) -> (Vec<(usize, &T)>, usize) {
    let mut ordered: Vec<(usize, &T)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            let value = weight(row);
            value.is_finite() && value > 0.0
        })
        .collect();
    ordered.sort_by(|a, b| weight(b.1).total_cmp(&weight(a.1)));
    let total = ordered.len();
    (ordered, total)
}

/// Where the drawn row landed in the rendered list.
///
/// Returns `None` when the caller named no draw or named a nonpositive row.
fn drawn_index<T>(kept: &[(usize, &T)], drawn: Option<usize>) -> Option<usize> {
    let drawn = drawn?;
    kept.iter().position(|(index, _)| *index == drawn)
}

/// Names the roster entries that one index list selects.
///
/// The session holds P2's own team, so every species is known. An index outside
/// the roster cannot happen, and the fallback keeps the row complete.
fn roster_names(
    mons: &[poke_rust::state::pokemon::PokemonState],
    indices: &[usize],
) -> Vec<String> {
    indices
        .iter()
        .map(|&index| match mons.get(index) {
            Some(mon) => mon.species.to_string(),
            None => "Unknown".to_string(),
        })
        .collect()
}

/// Starts one analysis job for the current position.
///
/// Does nothing when the session holds no P2 profile, and nothing after the
/// game ends. A team preview and a battle both get a job, so the bot searches
/// its leads as it searches every later turn. The caller holds the session
/// lock, so this call only spawns the task. The task takes the lock itself
/// after the search returns.
pub fn start_job(
    battle_id: &str,
    session: &mut BattleSession,
    dexes: Arc<Dexes>,
    meta: Arc<MetaDexes>,
    sessions: Arc<Mutex<HashMap<String, BattleSession>>>,
) {
    let Some(profile) = session.bot_p2.as_ref() else {
        return;
    };
    if matches!(session.state, MatchState::GameOverState { .. }) {
        // `routes::submit_turn` calls this after the turn resolves, so a
        // finished game reaches this exit. That exit is correct, and the client
        // asks for no further turn. Report a normal end as a normal end.
        eprintln!(
            "analysis: no job started at generation {}, because the battle is over",
            session.analysis.generation()
        );
        return;
    }

    let search = profile.search;
    let seed = profile.view.seed.unwrap_or_else(random_seed);
    let format = MetaFormat::from_active_per_side(session.config.active_per_side);
    let state = session.state.clone();
    let belief_p2 = session.belief_p2.clone();
    let generation = session.analysis.generation;
    // Only a belief search reads the inference rules, and cloning the learnset
    // dex is not free, so build the copy for those two algorithms alone.
    let inference = search
        .searches_belief()
        .then(|| {
            session
                .inference_config
                .as_ref()
                .map(clone_inference_config)
        })
        .flatten();

    let replay = AnalysisReplay {
        generation,
        turn_number: 0,
        search_seed: seed,
        algorithm: profile.view.algorithm.clone(),
        simulation_turn_budget: profile.view.simulation_turn_budget,
        depth: profile.view.depth,
        replacement_depth: profile.view.replacement_depth,
        workers: profile.view.workers,
        particles: profile.view.particles,
        damage_rolls: profile.view.damage_rolls,
        consider_crit: profile.view.consider_crit,
        turns_simulated: 0,
    };

    let job = session
        .analysis
        .start_with_budget(profile.view.simulation_turn_budget);
    if let Some(checkpoint) = forced_checkpoint(session, &dexes, seed, replay.clone()) {
        let line = session.analysis.accept(&job, Ok(checkpoint));
        eprintln!("{line}");
        return;
    }
    let battle_id = battle_id.to_string();
    eprintln!(
        "analysis job {} (generation {}): start, algorithm {}, depth {}, simulation-turn budget {}",
        job.id, job.generation, replay.algorithm, replay.depth, replay.simulation_turn_budget
    );

    tokio::task::spawn_blocking(move || {
        // A cancel before the search saves the whole search.
        if job.cancel.is_cancelled() && !job.should_publish_cancelled_result() {
            eprintln!(
                "analysis job {} (generation {}): cancelled before the search started",
                job.id, job.generation
            );
            return;
        }
        let progress_sessions = Arc::clone(&sessions);
        let progress_battle_id = battle_id.clone();
        let publish = |checkpoint: AnalysisCheckpoint| {
            let mut sessions = progress_sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(session) = sessions.get_mut(&progress_battle_id) {
                session.analysis.publish(&job, checkpoint);
            }
        };
        let outcome = run_search(
            search,
            seed,
            generation,
            &state,
            belief_p2.as_ref(),
            &dexes,
            meta.for_format(format),
            inference,
            replay,
            &job.cancel,
            Some(&publish),
        );
        // A state change can cancel this job after it publishes a checkpoint.
        // The new generation must keep control of the session record.
        // The ticket check in `accept` protects the later race.
        if job.cancel.is_cancelled() && !job.should_publish_cancelled_result() {
            eprintln!(
                "analysis job {} (generation {}): cancelled during the search, \
                 so it reports no result",
                job.id, job.generation
            );
            return;
        }
        let mut sessions = sessions.lock().unwrap_or_else(|e| e.into_inner());
        // A deleted battle leaves the task with no target.
        let line = if let Some(session) = sessions.get_mut(&battle_id) {
            session.analysis.accept(&job, outcome)
        } else {
            format!(
                "analysis job {} (generation {}): the battle is gone, \
                 so it reports no result",
                job.id, job.generation
            )
        };
        // A console write can block. The operator can pause a Windows console,
        // and a redirected stream can fill its pipe. This lock guards every
        // session, so release it before the write.
        drop(sessions);
        eprintln!("{line}");
    });
}

/// Draws the seed of one job.
///
/// The checkpoint publishes this number, and the client reads it as a JSON
/// number, so it must survive a round trip through a JavaScript double.
/// `bot::resolve` rejects a submitted seed above [`MAX_SAFE_INTEGER`] for the
/// same reason, and a seed the server itself draws has to satisfy the rule the
/// server enforces. That constant is `2^53 - 1`, so the mask is uniform.
fn random_seed() -> u64 {
    rand::random::<u64>() & MAX_SAFE_INTEGER
}

/// Returns an immediate checkpoint when Player 2 has one legal choice.
fn forced_checkpoint(
    session: &BattleSession,
    dexes: &Dexes,
    seed: u64,
    mut replay: AnalysisReplay,
) -> Option<AnalysisCheckpoint> {
    let (turn_number, strategy) = match &session.state {
        MatchState::BattleState(battle) => {
            let joint = solver::actions::joint_actions(
                battle,
                Player::P2,
                solver::actions::phase_of(&session.state),
                &dexes.move_dex,
                &dexes.pokemon_dex,
                None,
                false,
            );
            let legal: Vec<Vec<BattleCommand>> = joint
                .actions
                .into_iter()
                .filter(|commands| accept(session, dexes, commands).is_some())
                .collect();
            if legal.len() != 1 {
                return None;
            }
            (
                battle.turn_number,
                P2Strategy::Battle(vec![JointActionProb {
                    commands: legal.into_iter().next().unwrap(),
                    probability: 1.0,
                }]),
            )
        }
        MatchState::TeamPreviewState(preview) => {
            let legal: Vec<TeamPreviewCommand> =
                solver::preview::preview_choices(preview, Player::P2)
                    .into_iter()
                    .filter(|choice| accept_preview(session, dexes, choice).is_some())
                    .collect();
            if legal.len() != 1 {
                return None;
            }
            (
                0,
                P2Strategy::Preview(vec![PreviewChoiceProb {
                    choice: legal.into_iter().next().unwrap(),
                    probability: 1.0,
                }]),
            )
        }
        MatchState::GameOverState { .. } => return None,
    };
    replay.turn_number = turn_number;
    Some(AnalysisCheckpoint {
        generation: session.analysis.generation(),
        turn_number,
        p2_win_odds: 0.5,
        complete: true,
        p2_strategy: strategy,
        depth_reached: 0,
        turns_simulated: 0,
        nodes: 0,
        elapsed: Duration::ZERO,
        seed,
        replay,
        strategy_is_playable: true,
        warnings: vec!["Player 2 has one legal choice, so the solver did not search.".to_string()],
    })
}

/// Copies the inference rules of a session for a belief search.
///
/// `InferenceConfig` lives in the engine, and the determinizer needs an owned
/// copy per job. `tracker_analysis.rs` copies its own session rules the same
/// way.
pub fn clone_inference_config(config: &InferenceConfig) -> InferenceConfig {
    InferenceConfig {
        use_stat_points: config.use_stat_points,
        force_max_ivs: config.force_max_ivs,
        level: config.level,
        legal_items: config.legal_items.clone(),
        allow_repeat_items: config.allow_repeat_items,
        learnset_dex: config.learnset_dex.clone(),
        ev_total_cap: config.ev_total_cap,
    }
}

/// Runs one search and converts its result to a checkpoint.
///
/// A configuration that this session cannot serve returns an error, so the
/// session keeps its last complete checkpoint.
#[allow(clippy::too_many_arguments)]
fn run_search(
    search: BotSearchConfig,
    seed: u64,
    generation: u64,
    state: &MatchState,
    belief_p2: Option<&UnknownMatchState>,
    dexes: &Dexes,
    meta: Option<&MetaDex>,
    inference: Option<InferenceConfig>,
    replay: AnalysisReplay,
    cancel: &CancelFlag,
    progress: Option<&dyn Fn(AnalysisCheckpoint)>,
) -> Result<AnalysisCheckpoint, String> {
    let turn_number = match state {
        MatchState::BattleState(battle) => battle.turn_number,
        // The preview runs before turn one, so it carries turn zero.
        MatchState::TeamPreviewState(_) => 0,
        _ => {
            return Err(
                "the analysis job runs only on a preview position or a battle position".to_string(),
            );
        }
    };
    let started = Instant::now();

    // A solver panic must not poison the session mutex, so catch it here and
    // report it as an ordinary job failure.
    let publish = |mut checkpoint: AnalysisCheckpoint| {
        decorate_checkpoint(
            &mut checkpoint,
            generation,
            turn_number,
            seed,
            &replay,
            search,
            belief_p2.is_some(),
        );
        if let Some(progress) = progress {
            progress(checkpoint);
        }
    };
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match state {
        MatchState::TeamPreviewState(preview) => one_preview_search(
            search,
            seed,
            preview,
            belief_p2,
            dexes,
            meta,
            inference,
            cancel,
            Some(&publish),
        ),
        _ => one_search(
            search,
            seed,
            state,
            belief_p2,
            dexes,
            meta,
            inference,
            cancel,
            Some(&publish),
        ),
    }));
    let mut checkpoint = match caught {
        Ok(result) => result?,
        Err(payload) => return Err(panic_message(payload)),
    };

    decorate_checkpoint(
        &mut checkpoint,
        generation,
        turn_number,
        seed,
        &replay,
        search,
        belief_p2.is_some(),
    );
    let _ = started;
    Ok(checkpoint)
}

fn decorate_checkpoint(
    checkpoint: &mut AnalysisCheckpoint,
    generation: u64,
    turn_number: u16,
    seed: u64,
    replay: &AnalysisReplay,
    search: BotSearchConfig,
    fogged: bool,
) {
    checkpoint.generation = generation;
    checkpoint.turn_number = turn_number;
    checkpoint.seed = seed;
    checkpoint.replay = AnalysisReplay {
        turn_number,
        turns_simulated: checkpoint.turns_simulated,
        ..replay.clone()
    };
    checkpoint.strategy_is_playable = strategy_respects_fog(search, fogged);
    if let Some(line) = perfect_information_warning(search, fogged)
        && !checkpoint.warnings.contains(&line)
    {
        checkpoint.warnings.push(line);
    }
}

/// Routes one resolved profile to its solver entry point.
///
/// The returned checkpoint carries no generation, no turn number, and no seed.
/// [`run_search`] fills those three fields.
///
/// Every arm passes `cancel`, so a raised flag stops the search that runs.
#[allow(clippy::too_many_arguments)]
fn one_search(
    search: BotSearchConfig,
    seed: u64,
    state: &MatchState,
    belief_p2: Option<&UnknownMatchState>,
    dexes: &Dexes,
    meta: Option<&MetaDex>,
    inference: Option<InferenceConfig>,
    cancel: &CancelFlag,
    progress: Option<&dyn Fn(AnalysisCheckpoint)>,
) -> Result<AnalysisCheckpoint, String> {
    let cancel = Some(cancel);
    match search {
        BotSearchConfig::Exact(config) => {
            let round = |round: solver::RootRound| {
                let mut checkpoint = partial_checkpoint(
                    1.0 - round.value,
                    P2Strategy::Battle(round.p2_strategy),
                    round.depth,
                    round.stats.turns_simulated,
                    round.stats.nodes_expanded,
                    round.stats.elapsed,
                    &[],
                    &[],
                );
                checkpoint.complete = false;
                if let Some(progress) = progress {
                    progress(checkpoint);
                }
            };
            let result = solver::solve_seeded_progress_cancellable(
                seed,
                state,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                Some(&round),
                cancel,
            )
            .map_err(engine_error)?;
            let mut checkpoint = partial_checkpoint(
                result.p2_win_odds,
                P2Strategy::Battle(result.p2_strategy),
                result.depth_reached,
                result.stats.turns_simulated,
                result.stats.nodes_expanded,
                result.stats.elapsed,
                &result.warnings,
                &[],
            );
            checkpoint.complete =
                result.depth_reached == config.depth && solver::warnings_are_complete(&result.warnings);
            Ok(checkpoint)
        }
        BotSearchConfig::Mcts(config) => {
            let sampled = |root: solver::mcts::SampledRoot| {
                let mut checkpoint = partial_checkpoint(
                    1.0 - root.value,
                    P2Strategy::Battle(root.p2_strategy),
                    config.depth,
                    root.stats.turns_simulated,
                    root.stats.nodes_created,
                    root.stats.elapsed,
                    &[],
                    &[],
                );
                checkpoint.complete = false;
                if let Some(progress) = progress {
                    progress(checkpoint);
                }
            };
            let result = solver::mcts::search_progress_cancellable(
                seed,
                state,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                Some(&sampled),
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                P2Strategy::Battle(result.p2_strategy),
                config.depth,
                result.stats.turns_simulated,
                result.stats.nodes_created,
                result.stats.elapsed,
                &result.warnings,
                &[],
            ))
        }
        BotSearchConfig::Ismcts(config) => {
            let (belief, determinize) = belief_search_inputs(belief_p2, inference)?;
            let meta = meta.ok_or_else(unsupported_meta)?;
            let sampled = |root: solver::mcts::SampledRoot| {
                let mut checkpoint = partial_checkpoint(
                    1.0 - root.value,
                    P2Strategy::Battle(root.p2_strategy),
                    config.search.depth,
                    root.stats.turns_simulated,
                    root.stats.nodes_created,
                    root.stats.elapsed,
                    &[],
                    &[],
                );
                checkpoint.complete = false;
                if let Some(progress) = progress {
                    progress(checkpoint);
                }
            };
            let result = solver::ismcts::search_belief_progress_cancellable(
                seed,
                belief,
                meta,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                &determinize,
                Some(&sampled),
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                P2Strategy::Battle(result.p2_strategy),
                config.search.depth,
                result.stats.turns_simulated,
                result.stats.nodes_created,
                result.stats.elapsed,
                &result.warnings,
                &result.draw_warnings,
            ))
        }
        BotSearchConfig::Pimc(config) => {
            let (belief, determinize) = belief_search_inputs(belief_p2, inference)?;
            let meta = meta.ok_or_else(unsupported_meta)?;
            let result = solver::pimc::search_belief_cancellable(
                seed,
                belief,
                meta,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                &determinize,
                // The bot job publishes one checkpoint at the end, so it reads
                // no per-world answer.
                None,
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                P2Strategy::Battle(result.p2_strategy),
                result.depth_reached,
                result.stats.turns_simulated,
                result.stats.nodes_expanded,
                result.stats.elapsed,
                &result.warnings,
                &result.draw_warnings,
            ))
        }
        BotSearchConfig::Mccfr(config) => {
            let (belief, determinize) = belief_search_inputs(belief_p2, inference)?;
            let meta = meta.ok_or_else(unsupported_meta)?;
            let sampled = |root: solver::mcts::SampledRoot| {
                let mut checkpoint = partial_checkpoint(
                    1.0 - root.value,
                    P2Strategy::Battle(root.p2_strategy),
                    config.search.depth,
                    root.stats.turns_simulated,
                    root.stats.nodes_created,
                    root.stats.elapsed,
                    &[],
                    &[],
                );
                checkpoint.complete = false;
                if let Some(progress) = progress {
                    progress(checkpoint);
                }
            };
            let result = solver::mccfr::search_belief_progress_cancellable(
                seed,
                belief,
                meta,
                &dexes.pokemon_dex,
                &dexes.move_dex,
                &config,
                &determinize,
                Some(&sampled),
                cancel,
            )
            .map_err(engine_error)?;
            Ok(partial_checkpoint(
                result.p2_win_odds,
                P2Strategy::Battle(result.p2_strategy),
                config.search.depth,
                result.stats.turns_simulated,
                result.stats.nodes_created,
                result.stats.elapsed,
                &result.warnings,
                &result.draw_warnings,
            ))
        }
    }
}

/// Names the guess that a one-world preview search rests on.
///
/// The preview solver reads concrete worlds rather than the belief, so a
/// fog-of-war session with one world scores every cell against one guess of
/// P1's hidden data. The line names the algorithm names that the client already
/// holds through the profile, and no state of either side.
const PREVIEW_ONE_WORLD_NOTE: &str = "The search drew one world of the belief, so the whole \
     answer assumes one guess of the opponent's hidden data. Only ismcts, mccfr, and pimc draw \
     more than one world.";

/// Names the limit that a many-world preview answer carries.
///
/// The open-list solve takes the mean cell value across the drawn worlds, so
/// one strategy covers every world. A real opponent conditions on its own
/// hidden data, which this answer cannot.
const PREVIEW_MEAN_MATRIX_NOTE: &str = "The preview search solved the mean matrix of the \
     drawn worlds, so one strategy covers every world. A real opponent can play a different \
     lead in each world.";

/// Runs one preview search and converts its result to a checkpoint.
///
/// The position selects the entry point:
///
/// - A fog-of-war session with a belief search solves P2's preview belief
///   through the open list, so the answer reads only what P2 may hold. That
///   solve averages the payoff matrix of the drawn worlds, whatever the profile
///   does at a battle position.
/// - Every other session solves the true preview state. On a fog-of-war session
///   [`strategy_respects_fog`] then marks the answer as not playable, and
///   [`draw_p2_command`] falls through to its uniform case. This is the rule
///   that a battle position already applies.
///
/// The preview solver has no depth ladder, so the checkpoint reports the depth
/// of the battle solve below each cell.
#[allow(clippy::too_many_arguments)]
fn one_preview_search(
    search: BotSearchConfig,
    seed: u64,
    preview: &TeamPreviewState,
    belief_p2: Option<&UnknownMatchState>,
    dexes: &Dexes,
    meta: Option<&MetaDex>,
    inference: Option<InferenceConfig>,
    cancel: &CancelFlag,
    progress: Option<&dyn Fn(AnalysisCheckpoint)>,
) -> Result<AnalysisCheckpoint, String> {
    let battle = preview_battle_config(search);
    let depth = battle.depth;
    let config = PreviewConfig {
        battle,
        deadline: None,
    };

    // Only a belief algorithm reads the belief, and a belief algorithm reads
    // nothing else. A battle position takes the same two rules through
    // `belief_search_inputs`, so a missing belief is an error rather than a
    // silent fall through to the true state.
    if search.searches_belief() {
        let Some(UnknownMatchState::TeamPreview(belief)) = belief_p2 else {
            return Err(NO_FOG_OF_WAR.to_string());
        };
        let Some(inference) = inference else {
            return Err(NO_FOG_OF_WAR.to_string());
        };
        let meta = meta.ok_or_else(unsupported_meta)?;
        let worlds = preview_worlds(search);
        let open_list = OpenListConfig {
            preview: config,
            worlds,
            seed,
        };
        let determinize = DeterminizeConfig {
            inference,
            observer: Player::P2,
            ..DeterminizeConfig::default()
        };
        let publish_round = |round: PreviewRound| {
            let mut checkpoint = partial_checkpoint(
                1.0 - round.value,
                P2Strategy::Preview(round.p2_strategy),
                round.depth,
                round.stats.turns_simulated,
                round.stats.cells_evaluated,
                round.stats.elapsed,
                &[],
                &[],
            );
            checkpoint.complete = false;
            checkpoint.warnings.push(if worlds == 1 {
                PREVIEW_ONE_WORLD_NOTE.to_string()
            } else {
                PREVIEW_MEAN_MATRIX_NOTE.to_string()
            });
            if let Some(progress) = progress {
                progress(checkpoint);
            }
        };
        let result = solve_open_list_preview_progress_cancellable(
            belief,
            meta,
            &dexes.pokemon_dex,
            &dexes.move_dex,
            &open_list,
            &determinize,
            Some(&publish_round),
            Some(cancel),
        )
        .map_err(engine_error)?;
        let mut checkpoint = partial_checkpoint(
            result.p2_win_odds,
            P2Strategy::Preview(result.p2_strategy),
            result.depth_reached,
            result.stats.turns_simulated,
            result.stats.cells_evaluated,
            result.stats.elapsed,
            &result.warnings,
            &result.draw_warnings,
        );
        checkpoint.complete =
            preview_answer_is_complete(depth, result.depth_reached, &result.warnings);
        checkpoint.warnings.push(if worlds == 1 {
            PREVIEW_ONE_WORLD_NOTE.to_string()
        } else {
            PREVIEW_MEAN_MATRIX_NOTE.to_string()
        });
        return Ok(checkpoint);
    }

    let publish_round = |round: PreviewRound| {
        let mut checkpoint = partial_checkpoint(
            1.0 - round.value,
            P2Strategy::Preview(round.p2_strategy),
            round.depth,
            round.stats.turns_simulated,
            round.stats.cells_evaluated,
            round.stats.elapsed,
            &[],
            &[],
        );
        checkpoint.complete = false;
        if let Some(progress) = progress {
            progress(checkpoint);
        }
    };
    let result = solve_team_preview_progress_cancellable(
        preview,
        &dexes.pokemon_dex,
        &dexes.move_dex,
        &config,
        Some(&publish_round),
        Some(cancel),
    )
    .map_err(engine_error)?;
    let mut checkpoint = partial_checkpoint(
        result.p2_win_odds,
        P2Strategy::Preview(result.p2_strategy),
        result.depth_reached,
        result.stats.turns_simulated,
        result.stats.cells_evaluated,
        result.stats.elapsed,
        &result.warnings,
        &[],
    );
    checkpoint.complete = preview_answer_is_complete(depth, result.depth_reached, &result.warnings);
    Ok(checkpoint)
}

/// True when a preview answer completed the requested battle depth.
fn preview_answer_is_complete(
    target_depth: u8,
    depth_reached: u8,
    warnings: &[solver::SolveWarning],
) -> bool {
    depth_reached == target_depth && solver::warnings_are_complete(warnings)
}

/// Reports a search that read more than the fog of war allows.
///
/// `Exact` and `Mcts` search the true `MatchState`, so on a fog-of-war session
/// they read P1's unrevealed moves, item, ability, and spread. Only a search
/// that [`BotSearchConfig::searches_belief`] names draws its worlds from P2's
/// belief. The profile's `approximations`
/// list every way the answer falls short of exact and no way it overshoots, so
/// name this one on the checkpoint.
///
/// The line names the algorithm, which the client already reads back in
/// `BattleView::bot_p2`, and no state of either side.
fn perfect_information_warning(search: BotSearchConfig, fogged: bool) -> Option<String> {
    (!strategy_respects_fog(search, fogged)).then(|| {
        "This algorithm searched the true position, so the answer used data that the fog of \
         war hides. Only ismcts, mccfr, and pimc search the belief."
            .to_string()
    })
}

/// True when this search can control P2 without reading hidden P1 data.
fn strategy_respects_fog(search: BotSearchConfig, fogged: bool) -> bool {
    !fogged || search.searches_belief()
}

/// Reports a belief search that this session cannot serve.
///
/// A belief search draws its worlds from the belief of the observer, and a
/// perfect-information session builds no belief. The line names the profile
/// field alone.
const NO_FOG_OF_WAR: &str =
    "botP2: this algorithm needs a fog-of-war battle, and this session has none";

/// The belief and the draw rules of a belief search.
///
/// P2 is the observer, so the determinizer copies P2's own side and samples
/// P1's hidden data.
fn belief_search_inputs(
    belief_p2: Option<&UnknownMatchState>,
    inference: Option<InferenceConfig>,
) -> Result<
    (
        &poke_rust::information::unknowns::UnknownBattleState,
        DeterminizeConfig,
    ),
    String,
> {
    let Some(UnknownMatchState::Battle(belief)) = belief_p2 else {
        return Err(NO_FOG_OF_WAR.to_string());
    };
    let Some(inference) = inference else {
        return Err(NO_FOG_OF_WAR.to_string());
    };
    let determinize = DeterminizeConfig {
        inference,
        observer: Player::P2,
        ..DeterminizeConfig::default()
    };
    Ok((belief, determinize))
}

fn unsupported_meta() -> String {
    "botP2: this algorithm draws worlds from usage data, and no usage cache is loaded \
     (see meta_scraper/README.md)"
        .to_string()
}

/// Converts an engine error to a job message.
///
/// The engine writes its own text, and that text names hidden data: a
/// [`DeterminizeError`](poke_rust::information::determinize::DeterminizeError)
/// prints a species and a belief mon index, and a belief error wraps one. P1
/// reads `GET /api/battles/{id}/analysis` during a hotseat battle, so the
/// response carries a fixed line and the detail goes to the server console,
/// where only the operator reads it.
///
/// The messages that this module writes itself name no state, so those still
/// reach the client whole.
fn engine_error(error: impl std::fmt::Display) -> String {
    eprintln!("analysis job: the search failed: {error}");
    "The search failed. The server console holds the reason.".to_string()
}

/// Converts a caught panic payload to a job error message.
///
/// A panic payload is engine text too, so it goes to the console for the same
/// reason as [`engine_error`].
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no payload".to_string());
    eprintln!("analysis job: the search panicked: {detail}");
    "The search panicked. The server console holds the reason.".to_string()
}

/// Renders one solver warning for the progress view.
///
/// The line never names a player and never carries an action count. P1 reads
/// the same endpoint, so `ActionsTruncated` would otherwise report the size of
/// P2's action set. Every other figure is either a limit that the client already
/// holds through the profile or a depth that the view already reports.
///
/// Scrubbing the player name makes the two `ActionsTruncated` lines equal, so
/// [`partial_checkpoint`] also drops the repeat. See the note there.
fn warning_line(warning: &solver::SolveWarning) -> String {
    match warning {
        solver::SolveWarning::BudgetExhausted { budget } => {
            format!("The {budget}-node budget ran out, so deeper nodes took a static score.")
        }
        solver::SolveWarning::SimulationTurnBudgetExhausted { budget } => format!(
            "The search exhausted the {budget}-turn simulation budget. Later positions used static scores."
        ),
        solver::SolveWarning::DeadlineExceeded { budget } => format!(
            "The {} ms limit expired, so deeper nodes took a static score.",
            budget.as_millis()
        ),
        solver::SolveWarning::DepthNotReached { target, reached } => {
            format!("The search completed depth {reached} of the {target} requested.")
        }
        solver::SolveWarning::ChanceMassDiscarded { max_fraction } => format!(
            "One chance node discarded up to {:.1}% of its outcome probability.",
            100.0 * max_fraction
        ),
        solver::SolveWarning::ActionsTruncated { .. } => {
            "The action cap removed at least one action, so the search can miss it.".to_string()
        }
        solver::SolveWarning::NoCompletedRound => "The search completed no equilibrium round, so both strategies are an even split over the actions rather than a calculated mixture."
            .to_string(),
        // A cancelled job reports no checkpoint, so this line reaches the client
        // only through a search that a caller inside the process cancelled. The
        // line names no state of either side.
        solver::SolveWarning::Cancelled => {
            "The search was cancelled, so the answer holds only the work that finished.".to_string()
        }
        // The world count is a limit that the client already holds through the
        // profile, so this line carries no new data about either side.
        solver::SolveWarning::StrategyFusion { worlds } => format!(
            "The search solved {worlds} world(s) separately and averaged the strategies. Each \
             world played as if the hidden data were known (strategy fusion), so the answer \
             claims more than a real player can do."
        ),
    }
}

/// Builds the shared part of a checkpoint from one solver result.
///
/// Repeated lines collapse to one. The solver raises `ActionsTruncated` once
/// per player, and [`warning_line`] scrubs the player name, so both players
/// render the same line. The count of that line would then carry the fact that
/// the scrub removes: P1 holds its own action count and the cap, so two lines
/// say P2 exceeded the cap and one line says P2 did not. One line for each
/// distinct message says only that some action was cut.
#[allow(clippy::too_many_arguments)]
fn partial_checkpoint(
    p2_win_odds: f64,
    p2_strategy: P2Strategy,
    depth_reached: u8,
    turns_simulated: u64,
    nodes: u64,
    elapsed: Duration,
    warnings: &[solver::SolveWarning],
    draw_warnings: &[poke_rust::information::determinize::DeterminizeWarning],
) -> AnalysisCheckpoint {
    let mut seen = std::collections::HashSet::new();
    let mut lines: Vec<String> = warnings
        .iter()
        .map(warning_line)
        .filter(|line| seen.insert(line.clone()))
        .collect();
    if !draw_warnings.is_empty() {
        lines.push(format!(
            "The determinizer reported {} warning(s) while it drew the hidden side.",
            draw_warnings.len()
        ));
    }
    AnalysisCheckpoint {
        generation: 0,
        turn_number: 0,
        p2_win_odds,
        complete: solver::sampling_warnings_are_complete(warnings),
        p2_strategy,
        depth_reached,
        turns_simulated,
        nodes,
        elapsed,
        seed: 0,
        replay: AnalysisReplay::default(),
        strategy_is_playable: true,
        warnings: lines,
    }
}

// ── The P2 draw ──────────────────────────────────────────────────────────────

/// Which rule produced the drawn P2 command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawSource {
    /// The mixed strategy of a checkpoint of the current position.
    Strategy,
    /// A uniform draw over the legal joint actions.
    Uniform,
    /// A uniform draw over the team-preview choices.
    TeamPreview,
}

impl DrawSource {
    fn as_str(self) -> &'static str {
        match self {
            DrawSource::Strategy => "strategy",
            DrawSource::Uniform => "uniform",
            DrawSource::TeamPreview => "teamPreview",
        }
    }
}

/// One drawn P2 command.
///
/// The record holds one drawn action and the strategy that supplied it.
pub struct P2Draw {
    pub command: PlayerCommand,
    pub source: DrawSource,
    /// The seed of this draw.
    pub seed: u64,
    /// The replay record of the checkpoint that supplied the strategy.
    /// `None` for either uniform draw.
    pub replay: Option<AnalysisReplay>,
    /// The strategy that this draw sampled from.
    ///
    /// `None` for a uniform draw, which reads no strategy.
    pub strategy: Option<P2StrategyDto>,
}

/// Draws the command that P2 plays this turn.
///
/// A position with a checkpoint of that same position draws from the mixed
/// strategy of the checkpoint. Every other case draws one legal command with a
/// uniform weight. The uniform case covers a job that has not finished, a job
/// that failed, a strategy that read data the fog of war hides, and a strategy
/// whose command the legality check rejects.
///
/// [`P2Draw::source`] names the case that ran, so the client can show which one
/// it was. A uniform preview draw reports [`DrawSource::TeamPreview`], which
/// tells it apart from a uniform battle draw.
pub fn draw_p2_command(session: &BattleSession, dexes: &Dexes) -> Result<P2Draw, String> {
    let seed = draw_seed(session);
    let mut rng = StdRng::seed_from_u64(seed);

    if let MatchState::TeamPreviewState(preview) = &session.state {
        return draw_preview_command(session, dexes, preview, seed, &mut rng);
    }

    let MatchState::BattleState(battle) = &session.state else {
        return Err("battle is already over".to_string());
    };

    // The strategy case has five exits, and four of them fall through to the
    // uniform draw. Name the exit on the console. The reason reads P2's plan, so
    // it never reaches the client. See `engine_error`.
    let generation = session.analysis.generation();
    match playable_checkpoint(&session.analysis) {
        Err(reason) => uniform_draw_line(generation, reason),
        Ok(checkpoint) => match battle_rows(&checkpoint.p2_strategy)
            .and_then(|rows| sample_legal_strategy(session, dexes, rows, &mut rng))
        {
            None => uniform_draw_line(
                generation,
                "the strategy holds no legal action with a positive weight for this position",
            ),
            Some((index, command)) => {
                return Ok(P2Draw {
                    command,
                    source: DrawSource::Strategy,
                    seed,
                    replay: Some(checkpoint.replay.clone()),
                    strategy: revealed_strategy(session, checkpoint, index),
                });
            }
        },
    }

    // No cap and no dominance pruning: the draw needs the whole legal set, and
    // either filter would remove an action that P2 may play.
    let joint = solver::actions::joint_actions(
        battle,
        Player::P2,
        solver::actions::phase_of(&session.state),
        &dexes.move_dex,
        &dexes.pokemon_dex,
        None,
        false,
    );
    if joint.actions.is_empty() {
        return Err("P2 has no legal command right now".to_string());
    }
    let start = rng.gen_range(0..joint.actions.len());
    first_legal_from(session, dexes, &joint.actions, start)
        .map(|command| P2Draw {
            command,
            source: DrawSource::Uniform,
            seed,
            replay: None,
            strategy: None,
        })
        .ok_or_else(|| "P2 has no legal command right now".to_string())
}

/// Samples only strategy rows that the server accepts in the current state.
/// The remaining weights are normalized by the weighted draw.
fn sample_legal_strategy(
    session: &BattleSession,
    dexes: &Dexes,
    rows: &[JointActionProb],
    rng: &mut StdRng,
) -> Option<(usize, PlayerCommand)> {
    let legal: Vec<(usize, f64, PlayerCommand)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.probability.is_finite() && row.probability > 0.0)
        .filter_map(|(index, row)| {
            accept(session, dexes, &row.commands).map(|command| (index, row.probability, command))
        })
        .collect();
    let total: f64 = legal.iter().map(|(_, weight, _)| *weight).sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let mut roll = rng.gen_range(0.0..total);
    for (index, weight, command) in &legal {
        if roll < *weight {
            return Some((*index, command.clone()));
        }
        roll -= *weight;
    }
    legal
        .last()
        .map(|(index, _, command)| (*index, command.clone()))
}

/// Samples only preview rows that the server accepts in the current state.
/// The remaining weights are normalized by the weighted draw.
fn sample_legal_preview_strategy(
    session: &BattleSession,
    dexes: &Dexes,
    rows: &[PreviewChoiceProb],
    rng: &mut StdRng,
) -> Option<(usize, PlayerCommand)> {
    let legal: Vec<(usize, f64, PlayerCommand)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.probability.is_finite() && row.probability > 0.0)
        .filter_map(|(index, row)| {
            accept_preview(session, dexes, &row.choice)
                .map(|command| (index, row.probability, command))
        })
        .collect();
    let total: f64 = legal.iter().map(|(_, weight, _)| *weight).sum();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let mut roll = rng.gen_range(0.0..total);
    for (index, weight, command) in &legal {
        if roll < *weight {
            return Some((*index, command.clone()));
        }
        roll -= *weight;
    }
    legal
        .last()
        .map(|(index, _, command)| (*index, command.clone()))
}

/// The strategy that one draw sampled from, for a session that reveals it.
///
/// Returns `None` for every session that keeps the default setting, so the
/// reveal then carries the drawn command alone.
fn revealed_strategy(
    session: &BattleSession,
    checkpoint: &AnalysisCheckpoint,
    drawn: usize,
) -> Option<P2StrategyDto> {
    if !reveals_strategy(session) {
        return None;
    }
    strategy_dto(&session.state, &checkpoint.p2_strategy, Some(drawn))
}

/// The checkpoint that may supply P2's command at the current position.
///
/// Two rules reject a checkpoint. A checkpoint of an older position names
/// commands that the current position can no longer play. A strategy that read
/// data the fog of war hides must not control P2, whatever the position.
///
/// The error text is the console reason of the uniform draw. Both draws apply
/// these rules, so the preview position cannot become an exemption from either.
fn playable_checkpoint(analysis: &AnalysisState) -> Result<&AnalysisCheckpoint, &'static str> {
    match analysis.current_checkpoint() {
        None => Err("no checkpoint covers this position"),
        Some(checkpoint) if !checkpoint.strategy_is_playable => {
            Err("the strategy read data that the fog of war hides")
        }
        Some(checkpoint) => Ok(checkpoint),
    }
}

/// Writes the console reason of one uniform draw.
///
/// The reason reads P2's plan, so it never reaches the client. See
/// [`engine_error`].
fn uniform_draw_line(generation: u64, reason: &str) {
    eprintln!("analysis draw (generation {generation}): the draw is uniform, because {reason}");
}

/// The battle rows of a strategy.
///
/// A preview checkpoint returns `None`, so the battle draw falls through to its
/// uniform case rather than reading a choice that a battle turn cannot play.
fn battle_rows(strategy: &P2Strategy) -> Option<&[JointActionProb]> {
    match strategy {
        P2Strategy::Battle(rows) => Some(rows),
        P2Strategy::Preview(_) => None,
    }
}

/// The preview rows of a strategy.
///
/// A battle checkpoint returns `None`, for the reason [`battle_rows`] gives.
fn preview_rows(strategy: &P2Strategy) -> Option<&[PreviewChoiceProb]> {
    match strategy {
        P2Strategy::Preview(rows) => Some(rows),
        P2Strategy::Battle(_) => None,
    }
}

/// Draws the bring-and-lead choice that P2 plays.
///
/// The exits match the battle draw. A checkpoint of this preview supplies the
/// choice, and every other case draws one legal choice with a uniform weight.
fn draw_preview_command(
    session: &BattleSession,
    dexes: &Dexes,
    preview: &TeamPreviewState,
    seed: u64,
    rng: &mut StdRng,
) -> Result<P2Draw, String> {
    let generation = session.analysis.generation();
    match playable_checkpoint(&session.analysis) {
        Err(reason) => uniform_draw_line(generation, reason),
        Ok(checkpoint) => match preview_rows(&checkpoint.p2_strategy)
            .and_then(|rows| sample_legal_preview_strategy(session, dexes, rows, rng))
        {
            None => uniform_draw_line(
                generation,
                "the strategy holds no legal choice with a positive weight for this position",
            ),
            Some((index, command)) => {
                return Ok(P2Draw {
                    command,
                    source: DrawSource::Strategy,
                    seed,
                    replay: Some(checkpoint.replay.clone()),
                    strategy: revealed_strategy(session, checkpoint, index),
                });
            }
        },
    }

    let choices = solver::preview::preview_choices(preview, Player::P2);
    if choices.is_empty() {
        return Err("P2 has no legal team-preview choice".to_string());
    }
    let start = rng.gen_range(0..choices.len());
    (0..choices.len())
        .map(|offset| &choices[(start + offset) % choices.len()])
        .find_map(|choice| accept_preview(session, dexes, choice))
        .map(|command| P2Draw {
            command,
            source: DrawSource::TeamPreview,
            seed,
            replay: None,
            strategy: None,
        })
        .ok_or_else(|| "P2 has no legal team-preview choice".to_string())
}

/// Renders one drawn P2 command for the turn response.
///
/// `state` must be the position before the turn, so each description names the
/// Pokemon that acted.
///
/// The strategy block comes from the draw, which fills it only for a session
/// with `reveal_strategy`. A preview draw of such a session therefore names
/// P2's back picks, which is what that session asked for.
pub fn reveal_dto(state: &MatchState, draw: &P2Draw) -> P2RevealDto {
    // Team preview renders no command: the leads appear on the field of their
    // own accord, and the back picks stay hidden under the fog of war.
    let commands = match (state, &draw.command) {
        (MatchState::BattleState(battle), PlayerCommand::Battle(commands)) => commands
            .iter()
            .enumerate()
            .map(|(slot_idx, command)| {
                mapping::command_option(battle, Player::P2, slot_idx, command)
            })
            .collect(),
        _ => Vec::new(),
    };
    P2RevealDto {
        commands,
        source: draw.source.as_str().to_string(),
        draw_seed: draw.seed,
        replay: draw.replay.as_ref().map(replay_dto),
        strategy: draw.strategy.clone(),
    }
}

/// Builds the replay row of one reveal.
///
/// Each digest goes over the wire as a decimal string, because a `u64` above
/// `2^53` loses precision in a JavaScript number.
fn replay_dto(replay: &AnalysisReplay) -> AnalysisReplayDto {
    AnalysisReplayDto {
        generation: replay.generation,
        turn_number: replay.turn_number,
        search_seed: replay.search_seed,
        algorithm: replay.algorithm.clone(),
        simulation_turn_budget: replay.simulation_turn_budget,
        depth: replay.depth,
        replacement_depth: replay.replacement_depth,
        workers: replay.workers,
        particles: replay.particles,
        damage_rolls: replay.damage_rolls,
        consider_crit: replay.consider_crit,
        turns_simulated: replay.turns_simulated,
    }
}

/// The seed of one draw.
///
/// A profile seed makes the whole battle reproducible, so the generation mixes
/// into it. Without that mix one seed would select the same index of every
/// strategy, and P2 would repeat one action for the whole battle. A profile
/// with no seed draws a fresh one under the mask that [`random_seed`] explains.
fn draw_seed(session: &BattleSession) -> u64 {
    mix_draw_seed(
        session.bot_p2.as_ref().and_then(|p| p.view.seed),
        session.analysis.generation(),
    )
}

/// Mixes a profile seed with the generation of the position.
///
/// The result stays under [`MAX_SAFE_INTEGER`], because the reveal publishes it
/// and the client reads it as a JavaScript number.
fn mix_draw_seed(profile_seed: Option<u64>, generation: u64) -> u64 {
    match profile_seed {
        Some(seed) => (seed ^ generation.wrapping_mul(0x9E37_79B9_7F4A_7C15)) & MAX_SAFE_INTEGER,
        None => random_seed(),
    }
}

/// Draws one joint action from a mixed strategy.
///
/// Returns the index and the drawn row, so a reveal can identify it.
/// Returns `None` when no finite positive weight exists.
#[cfg(test)]
fn sample_strategy<'a>(
    strategy: &'a [JointActionProb],
    rng: &mut StdRng,
) -> Option<(usize, &'a [BattleCommand])> {
    sample_weighted(strategy, |action| action.probability, rng)
        .map(|(index, action)| (index, action.commands.as_slice()))
}

/// Draws one bring-and-lead choice from a preview strategy.
///
/// The index and the weight rules are those of [`sample_strategy`].
#[cfg(test)]
fn sample_preview<'a>(
    strategy: &'a [PreviewChoiceProb],
    rng: &mut StdRng,
) -> Option<(usize, &'a TeamPreviewCommand)> {
    sample_weighted(strategy, |row| row.probability, rng).map(|(index, row)| (index, &row.choice))
}

/// Draws one row of a mixed strategy.
///
/// Returns the index and the drawn row.
/// Returns `None` when no finite positive weight exists.
/// A negative weight counts as zero.
#[cfg(test)]
fn sample_weighted<'a, T>(
    rows: &'a [T],
    weight: impl Fn(&T) -> f64,
    rng: &mut StdRng,
) -> Option<(usize, &'a T)> {
    let mut total = 0.0;
    for row in rows {
        let value = weight(row);
        if !value.is_finite() {
            return None;
        }
        total += value.max(0.0);
    }
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let mut roll = rng.gen_range(0.0..1.0) * total;
    for (index, row) in rows.iter().enumerate() {
        let value = weight(row).max(0.0);
        if value > 0.0 && roll < value {
            return Some((index, row));
        }
        roll -= value;
    }
    rows.iter()
        .enumerate()
        .rev()
        .find(|(_, row)| weight(row) > 0.0)
}

/// The first joint action that the legality check accepts.
///
/// The scan starts at `start` and wraps, so one seed always gives one answer.
fn first_legal_from(
    session: &BattleSession,
    dexes: &Dexes,
    actions: &[Vec<BattleCommand>],
    start: usize,
) -> Option<PlayerCommand> {
    (0..actions.len())
        .map(|offset| &actions[(start + offset) % actions.len()])
        .find_map(|commands| accept(session, dexes, commands))
}

/// Runs one drawn joint action through the legality check of the server.
///
/// The check is the one that a submitted command takes, so a drawn command can
/// never reach the engine on a path that a client command could not.
fn accept(
    session: &BattleSession,
    dexes: &Dexes,
    commands: &[BattleCommand],
) -> Option<PlayerCommand> {
    let dto = PlayerCommandDto::Battle {
        commands: commands.iter().map(mapping::battle_command_dto).collect(),
    };
    session::reconstruct_player_command(session, dexes, Player::P2, &dto).ok()
}

/// Runs one drawn preview choice through the legality check of the server.
///
/// The check is the one that a submitted choice takes, for the reason
/// [`accept`] gives.
fn accept_preview(
    session: &BattleSession,
    dexes: &Dexes,
    choice: &TeamPreviewCommand,
) -> Option<PlayerCommand> {
    let dto = PlayerCommandDto::TeamPreview {
        active_indices: choice.active_indices.clone(),
        back_indices: choice.back_indices.clone(),
    };
    session::reconstruct_player_command(session, dexes, Player::P2, &dto).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use poke_rust::state::battle::{BattleCommand, SwitchCommand};

    fn checkpoint(generation: u64, turn_number: u16) -> AnalysisCheckpoint {
        AnalysisCheckpoint {
            generation,
            turn_number,
            p2_win_odds: 0.75,
            complete: true,
            p2_strategy: P2Strategy::Battle(vec![JointActionProb {
                commands: vec![BattleCommand::Pass],
                probability: 1.0,
            }]),
            depth_reached: 2,
            turns_simulated: 40,
            nodes: 12,
            elapsed: Duration::from_millis(30),
            seed: 7,
            replay: AnalysisReplay {
                generation,
                turn_number,
                search_seed: 7,
                algorithm: "doubleOracle".to_string(),
                simulation_turn_budget: 1_000,
                depth: 2,
                replacement_depth: None,
                workers: 1,
                particles: None,
                damage_rolls: 16,
                consider_crit: true,
                turns_simulated: 40,
            },
            strategy_is_playable: true,
            warnings: vec!["DepthNotReached".to_string()],
        }
    }

    #[test]
    fn invalidate_raises_the_generation_and_keeps_the_checkpoint() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 3)));
        assert_eq!(state.checkpoint().unwrap().turn_number, 3);

        state.invalidate();

        assert_eq!(state.generation, 1);
        assert_eq!(state.checkpoint().unwrap().turn_number, 3);
        let view = state.progress();
        assert_eq!(view.generation, 1);
        assert!(view.checkpoint.unwrap().stale);
    }

    #[test]
    fn finish_now_keeps_the_cancelled_jobs_result() {
        let mut state = AnalysisState::default();
        let job = state.start();

        state.finish_now().unwrap();

        assert!(job.cancel.is_cancelled());
        assert!(job.should_publish_cancelled_result());
        state.accept(&job, Ok(checkpoint(0, 1)));
        assert_eq!(state.current_checkpoint().unwrap().turn_number, 1);
    }

    #[test]
    fn invalidation_still_discards_an_early_result() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.finish_now().unwrap();
        state.invalidate();

        state.accept(&job, Ok(checkpoint(0, 1)));

        assert!(state.current_checkpoint().is_none());
    }

    #[test]
    fn accept_stores_a_result_of_the_current_generation() {
        let mut state = AnalysisState::default();
        state.invalidate();
        let job = state.start();

        state.accept(&job, Ok(checkpoint(1, 5)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 5);
        let view = state.progress();
        assert_eq!(view.phase, "complete");
        assert!(!view.checkpoint.unwrap().stale);
    }

    #[test]
    fn accept_drops_a_result_of_an_old_generation() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 1)));
        state.invalidate();

        state.accept(&job, Ok(checkpoint(0, 99)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 1);
        assert!(state.progress().error.is_none());
    }

    #[test]
    fn a_failed_job_keeps_the_last_complete_checkpoint() {
        let mut state = AnalysisState::default();
        let complete = state.start();
        state.accept(&complete, Ok(checkpoint(0, 2)));
        let failed = state.start();

        state.accept(&failed, Err("the search panicked".to_string()));

        assert_eq!(state.checkpoint().unwrap().turn_number, 2);
        let view = state.progress();
        assert_eq!(view.phase, "failed");
        assert_eq!(view.error.as_deref(), Some("the search panicked"));
        assert_eq!(view.checkpoint.unwrap().turn_number, 2);
    }

    #[test]
    fn invalidate_sets_the_cancel_flag_of_the_running_job() {
        let mut state = AnalysisState::default();
        let job = state.start();
        assert!(!job.cancel.is_cancelled());
        assert_eq!(state.progress().phase, "running");

        state.invalidate();

        assert!(job.cancel.is_cancelled());
        assert_eq!(state.progress().phase, "idle");
    }

    #[test]
    fn a_second_start_cancels_the_first_job() {
        let mut state = AnalysisState::default();
        let first = state.start();
        let second = state.start();

        assert!(first.cancel.is_cancelled());
        assert!(!second.cancel.is_cancelled());
    }

    /// A replaced job must not store a result or clear the new running job.
    #[test]
    fn accept_drops_a_result_from_a_replaced_job() {
        let mut state = AnalysisState::default();
        let first = state.start();
        let second = state.start();

        state.accept(&first, Ok(checkpoint(0, 1)));

        assert!(state.checkpoint().is_none());
        assert_eq!(state.progress().phase, "running");

        state.accept(&second, Ok(checkpoint(0, 2)));

        assert_eq!(state.checkpoint().unwrap().turn_number, 2);
        assert_eq!(state.progress().phase, "complete");
    }

    /// A clone of the flag shares the signal, which is what lets the session
    /// keep one handle while the search thread holds another.
    #[test]
    fn a_cloned_cancel_flag_shares_the_signal() {
        let flag = CancelFlag::new();
        let copy = flag.clone();

        assert!(!copy.is_cancelled());
        flag.cancel();

        assert!(copy.is_cancelled());
    }

    #[test]
    fn a_new_job_clears_the_last_error() {
        let mut state = AnalysisState::default();
        let failed = state.start();
        state.accept(&failed, Err("no usage cache".to_string()));
        assert_eq!(state.progress().phase, "failed");

        state.start();

        assert!(state.progress().error.is_none());
    }

    /// The state view carries odds but no rendered action row.
    #[test]
    fn the_progress_view_holds_no_p2_strategy() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 4)));

        let view = state.progress();
        let json = serde_json::to_string(&view).unwrap();

        assert!(!json.contains("Pass"), "{json}");
        assert!(!json.contains("strategy"), "{json}");
        assert!(json.contains("0.75"), "{json}");
        assert!(json.contains("p2WinOdds"), "{json}");
        assert!(!json.to_lowercase().contains("command"), "{json}");
        // Wall-clock cost is public.
        assert!(json.contains("elapsedMs"), "{json}");
    }

    /// The progress view exposes simulation turns but not solver nodes.
    #[test]
    fn the_progress_view_exposes_turns_but_not_nodes() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 4)));

        let json = serde_json::to_string(&state.progress()).unwrap();

        assert!(json.contains("\"turnsSimulated\":40"), "{json}");
        assert!(!json.contains("nodes"), "{json}");
        assert!(!json.contains("12"), "{json}");
    }

    /// The console line of a complete job must hold no action count either.
    ///
    /// P1 starts the server of a hotseat battle, so P1 can read that console.
    /// `p2_strategy.len()` is P2's joint-action count, which is the figure
    /// `warning_line` scrubs. Two strategies of different size must therefore
    /// write the same line.
    #[test]
    fn the_complete_line_holds_no_action_count() {
        let mut narrow = AnalysisState::default();
        let job = narrow.start();
        let short = narrow.accept(&job, Ok(checkpoint(0, 4)));

        let mut wide = AnalysisState::default();
        let job = wide.start();
        let mut many = checkpoint(0, 4);
        many.p2_strategy = P2Strategy::Battle(vec![
            JointActionProb {
                commands: vec![BattleCommand::Pass],
                probability: 1.0,
            };
            12
        ]);
        let long = wide.accept(&job, Ok(many));

        assert_eq!(short, long, "the line counts P2's actions");
        assert!(!short.contains("12"), "{short}");
        assert!(short.contains("the strategy holds an action"), "{short}");
    }

    /// An empty strategy explains the uniform draw, so the line names it.
    /// This one bit is not a count.
    #[test]
    fn the_complete_line_names_an_empty_strategy() {
        let mut state = AnalysisState::default();
        let job = state.start();
        let mut empty = checkpoint(0, 4);
        empty.p2_strategy = P2Strategy::Battle(Vec::new());

        let line = state.accept(&job, Ok(empty));

        assert!(line.contains("the strategy holds no action"), "{line}");
    }

    /// A dropped result and a failure both return their own line, so the caller
    /// writes the console line after it releases the session lock.
    #[test]
    fn each_accept_exit_returns_its_own_line() {
        let mut state = AnalysisState::default();
        let stale = state.start();
        state.invalidate();
        let dropped = state.accept(&stale, Ok(checkpoint(0, 1)));
        assert!(dropped.contains("the position moved"), "{dropped}");

        let first = state.start();
        let second = state.start();
        let replaced = state.accept(&first, Ok(checkpoint(1, 1)));
        assert!(replaced.contains("another job replaced it"), "{replaced}");

        let failed = state.accept(&second, Err("the search panicked".to_string()));
        assert!(failed.contains("no checkpoint"), "{failed}");
    }

    /// The published seed goes back over the wire as a JSON number, and
    /// `bot::resolve` rejects a submitted seed above `MAX_SAFE_INTEGER`. A seed
    /// the server draws itself has to pass the rule the server enforces.
    #[test]
    fn a_drawn_seed_survives_a_javascript_number() {
        for _ in 0..1_000 {
            let seed = random_seed();
            assert!(seed <= MAX_SAFE_INTEGER, "{seed}");
            assert_eq!(seed as f64 as u64, seed, "{seed}");
        }
    }

    /// `Exact` and `Mcts` search the true position, so on a fog-of-war session
    /// they read data that P2 cannot legally hold.
    #[test]
    fn a_perfect_information_search_on_a_fogged_session_says_so() {
        use poke_rust::solver::SolveConfig;
        use poke_rust::solver::mcts::MctsConfig;

        let exact = BotSearchConfig::Exact(SolveConfig::default());
        let mcts = BotSearchConfig::Mcts(MctsConfig::default());

        for search in [exact, mcts] {
            let line = perfect_information_warning(search, true)
                .expect("a fogged session must report the perfect-information read");
            assert!(line.contains("fog of war"), "{line}");
            // A perfect-information session hides nothing, so there is nothing
            // to report.
            assert_eq!(perfect_information_warning(search, false), None);
        }
    }

    /// A belief search reads only the belief, so it never earns the line.
    #[test]
    fn a_belief_search_reports_no_perfect_information_read() {
        use poke_rust::solver::ismcts::IsmctsConfig;
        use poke_rust::solver::mccfr::MccfrConfig;

        for search in [
            BotSearchConfig::Ismcts(IsmctsConfig::default()),
            BotSearchConfig::Mccfr(MccfrConfig::default()),
        ] {
            assert_eq!(perfect_information_warning(search, true), None);
        }
    }

    /// A true-state search can report progress in a fogged session, but its
    /// strategy must not control P2.
    #[test]
    fn a_true_state_strategy_cannot_control_a_fogged_bot() {
        let exact = BotSearchConfig::Exact(Default::default());
        let mcts = BotSearchConfig::Mcts(Default::default());
        let ismcts = BotSearchConfig::Ismcts(Default::default());
        let mccfr = BotSearchConfig::Mccfr(Default::default());

        assert!(!strategy_respects_fog(exact, true));
        assert!(!strategy_respects_fog(mcts, true));
        assert!(strategy_respects_fog(ismcts, true));
        assert!(strategy_respects_fog(mccfr, true));
        assert!(strategy_respects_fog(exact, false));
    }

    /// The engine writes a species and a belief mon index into its own error
    /// text, and P1 reads this endpoint during a hotseat battle.
    #[test]
    fn an_engine_error_reaches_the_client_without_its_detail() {
        use poke_rust::information::determinize::DeterminizeError;
        use poke_rust::solver::belief::BeliefError;
        use poke_rust::solver::ismcts::IsmctsError;

        let error = IsmctsError::Belief(BeliefError::Draw {
            world: 0,
            error: DeterminizeError::UnknownSpecies {
                mon_idx: 1,
                species: poke_rust::data::species::Species::Zoroark,
            },
        });
        let raw = error.to_string();
        assert!(raw.contains("Zoroark"), "{raw}");

        let message = engine_error(error);

        assert!(!message.contains("Zoroark"), "{message}");
        assert!(!message.contains("mon 1"), "{message}");
        assert!(message.contains("server console"), "{message}");
    }

    /// A panic payload is engine text too.
    #[test]
    fn a_panic_reaches_the_client_without_its_payload() {
        let payload = std::panic::catch_unwind(|| panic!("Zoroark: contradiction at mon 1"))
            .expect_err("the closure panics");

        let message = panic_message(payload);

        assert!(!message.contains("Zoroark"), "{message}");
        assert!(message.contains("server console"), "{message}");
    }

    /// `ActionsTruncated` names a player and its action count, and a
    /// determinizer warning names a Pokemon slot. Neither figure may reach P1.
    #[test]
    fn a_warning_line_holds_no_player_and_no_action_count() {
        let checkpoint = partial_checkpoint(
            0.5,
            P2Strategy::Battle(Vec::new()),
            1,
            0,
            0,
            Duration::ZERO,
            &[
                solver::SolveWarning::ActionsTruncated {
                    player: Player::P2,
                    kept: 6,
                    total: 37,
                },
                solver::SolveWarning::DepthNotReached {
                    target: 3,
                    reached: 1,
                },
            ],
            &[
                poke_rust::information::determinize::DeterminizeWarning::LearnsetTopUp {
                    mon_idx: 2,
                    moves: vec![poke_rust::data::pokemon_move::PokemonMove::Protect],
                },
            ],
        );
        let joined = checkpoint.warnings.join("\n");

        assert!(!joined.contains("P2"), "{joined}");
        assert!(!joined.contains("37"), "{joined}");
        assert!(!joined.contains("Protect"), "{joined}");
        assert_eq!(checkpoint.warnings.len(), 3);
        assert!(joined.contains("action cap"), "{joined}");
        assert!(joined.contains("depth 1 of the 3"), "{joined}");
        assert!(
            joined.contains("determinizer reported 1 warning"),
            "{joined}"
        );
        assert!(
            !checkpoint.complete,
            "the depth warning marks a partial answer"
        );
    }

    /// A sampling budget is terminal, but manual cancellation is partial.
    #[test]
    fn a_sampling_checkpoint_distinguishes_budget_and_cancellation() {
        let budget = partial_checkpoint(
            0.5,
            P2Strategy::Battle(Vec::new()),
            1,
            10,
            10,
            Duration::ZERO,
            &[solver::SolveWarning::SimulationTurnBudgetExhausted { budget: 10 }],
            &[],
        );
        let cancelled = partial_checkpoint(
            0.5,
            P2Strategy::Battle(Vec::new()),
            1,
            10,
            10,
            Duration::ZERO,
            &[solver::SolveWarning::Cancelled],
            &[],
        );

        assert!(budget.complete);
        assert!(!cancelled.complete);
    }

    /// The solver raises `ActionsTruncated` once per player, and the scrub makes
    /// both lines equal, so the count of that line replaces the player name that
    /// the scrub removed. P1 holds its own action count and the cap, so two
    /// lines would tell P1 that P2 exceeded the cap.
    #[test]
    fn a_repeated_warning_line_appears_one_time() {
        let both = |player_a, player_b| {
            partial_checkpoint(
                0.5,
                P2Strategy::Battle(Vec::new()),
                1,
                0,
                0,
                Duration::ZERO,
                &[
                    solver::SolveWarning::ActionsTruncated {
                        player: player_a,
                        kept: 6,
                        total: 37,
                    },
                    solver::SolveWarning::ActionsTruncated {
                        player: player_b,
                        kept: 6,
                        total: 12,
                    },
                ],
                &[],
            )
            .warnings
        };

        // Both players truncated.
        let two = both(Player::P1, Player::P2);
        assert_eq!(two.len(), 1, "{two:?}");

        // P1 alone truncated. The view must not tell the two cases apart.
        let one = partial_checkpoint(
            0.5,
            P2Strategy::Battle(Vec::new()),
            1,
            0,
            0,
            Duration::ZERO,
            &[solver::SolveWarning::ActionsTruncated {
                player: Player::P1,
                kept: 6,
                total: 37,
            }],
            &[],
        )
        .warnings;
        assert_eq!(one, two);
    }

    // ── The P2 draw ─────────────────────────────────────────────────────────

    fn strategy(weights: &[f64]) -> Vec<JointActionProb> {
        weights
            .iter()
            .enumerate()
            .map(|(idx, probability)| JointActionProb {
                commands: vec![BattleCommand::Switch(SwitchCommand { party_index: idx })],
                probability: *probability,
            })
            .collect()
    }

    /// The party index of one drawn switch row.
    fn drawn_party_index(drawn: (usize, &[BattleCommand])) -> usize {
        let commands = drawn.1;
        match &commands[0] {
            BattleCommand::Switch(switch) => switch.party_index,
            other => panic!("the fixture draws a switch, got {other:?}"),
        }
    }

    /// One seed must give one answer, so an operator can repeat a battle.
    #[test]
    fn one_seed_draws_the_same_action_every_time() {
        let strategy = strategy(&[0.3, 0.5, 0.2]);

        let first = {
            let mut rng = StdRng::seed_from_u64(4_242);
            drawn_party_index(sample_strategy(&strategy, &mut rng).unwrap())
        };
        for _ in 0..20 {
            let mut rng = StdRng::seed_from_u64(4_242);
            let again = drawn_party_index(sample_strategy(&strategy, &mut rng).unwrap());
            assert_eq!(again, first);
        }
    }

    /// The draw must follow the mixed strategy. A solver that plays its second
    /// action at 25% has to reach that rate at the table.
    #[test]
    fn the_draw_follows_the_strategy_weights() {
        let strategy = strategy(&[0.75, 0.25]);
        let mut rng = StdRng::seed_from_u64(1);
        let mut counts = [0u32; 2];

        for _ in 0..20_000 {
            counts[drawn_party_index(sample_strategy(&strategy, &mut rng).unwrap())] += 1;
        }

        let second = f64::from(counts[1]) / 20_000.0;
        assert!((second - 0.25).abs() < 0.02, "{counts:?}");
    }

    /// A short total can draw, but an invalid strategy must use the fallback.
    #[test]
    fn a_short_total_draws_and_an_invalid_total_does_not() {
        let mut rng = StdRng::seed_from_u64(9);

        // Weights that fall short of one.
        let short = strategy(&[0.4, 0.4]);
        assert!(sample_strategy(&short, &mut rng).is_some());

        // A zero total has no strategy row to draw.
        let zeroed = strategy(&[0.0, 0.0]);
        assert!(sample_strategy(&zeroed, &mut rng).is_none());

        // A non-finite total cannot define a mixed strategy.
        let non_finite = strategy(&[f64::NAN, 1.0]);
        assert!(sample_strategy(&non_finite, &mut rng).is_none());

        // A negative weight counts as zero, so the positive action wins.
        let negative = strategy(&[-1.0, 1.0]);
        for _ in 0..50 {
            assert_eq!(
                drawn_party_index(sample_strategy(&negative, &mut rng).unwrap()),
                1
            );
        }

        // An empty strategy has nothing to draw, which sends the caller to the
        // uniform case.
        assert!(sample_strategy(&[], &mut rng).is_none());
    }

    /// A stale checkpoint names actions of an older position, so the draw must
    /// not read it. The uniform case then runs.
    #[test]
    fn only_a_checkpoint_of_the_current_position_supplies_a_strategy() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(checkpoint(0, 1)));
        assert!(state.current_checkpoint().is_some());

        state.invalidate();

        assert!(state.checkpoint().is_some());
        assert!(state.current_checkpoint().is_none());
    }

    /// A job that never finished leaves no checkpoint at all.
    #[test]
    fn a_missing_checkpoint_supplies_no_strategy() {
        let mut state = AnalysisState::default();
        assert!(state.current_checkpoint().is_none());
        let job = state.start();

        state.accept(&job, Err("the search failed".to_string()));

        assert!(state.current_checkpoint().is_none());
    }

    /// A fixed profile seed must stay reproducible and must still move between
    /// turns. Without the mix, one seed would select the same index of every
    /// strategy for the whole battle.
    #[test]
    fn the_draw_seed_mixes_the_generation_in() {
        let first = mix_draw_seed(Some(77), 3);

        assert_eq!(first, mix_draw_seed(Some(77), 3));
        assert_ne!(first, mix_draw_seed(Some(77), 4));
        assert_ne!(first, mix_draw_seed(Some(78), 3));

        // The reveal publishes the seed as a JavaScript number.
        for generation in 0..1_000 {
            let seed = mix_draw_seed(Some(u64::MAX), generation);
            assert!(seed <= MAX_SAFE_INTEGER, "{seed}");
            assert_eq!(seed as f64 as u64, seed, "{seed}");
        }
    }

    /// Replay metadata must contain every resolved search setting. It must not
    /// contain a digest of hidden state.
    #[test]
    fn replay_metadata_is_complete_and_holds_no_hidden_state_digest() {
        let replay = AnalysisReplay {
            generation: 9,
            turn_number: 4,
            search_seed: 12,
            algorithm: "ismcts".to_string(),
            simulation_turn_budget: 40_000,
            depth: 3,
            replacement_depth: Some(2),
            workers: 1,
            particles: Some(32),
            damage_rolls: 16,
            consider_crit: true,
            turns_simulated: 20_000,
        };

        let json = serde_json::to_string(&replay_dto(&replay)).unwrap();

        assert!(json.contains("\"generation\":9"), "{json}");
        assert!(json.contains("\"turnNumber\":4"), "{json}");
        assert!(json.contains("\"particles\":32"), "{json}");
        assert!(json.contains("\"simulationTurnBudget\":40000"), "{json}");
        assert!(!json.to_lowercase().contains("hash"), "{json}");
        assert!(!json.to_lowercase().contains("belief"), "{json}");
        assert!(!json.to_lowercase().contains("state"), "{json}");
    }

    /// The reveal shows P2's one action. It must never show the odds of that
    /// action or any other action of the strategy.
    #[test]
    fn the_reveal_holds_no_probability_and_no_second_action() {
        let checkpoint = checkpoint(0, 3);
        let draw = P2Draw {
            command: PlayerCommand::Battle(vec![BattleCommand::Pass]),
            source: DrawSource::Strategy,
            seed: 31,
            replay: Some(checkpoint.replay.clone()),
            strategy: None,
        };
        // The fixture strategy holds one action; a real one holds more. Build
        // the reveal from the draw alone, which is the whole point.
        let reveal = P2RevealDto {
            commands: Vec::new(),
            source: draw.source.as_str().to_string(),
            draw_seed: draw.seed,
            replay: draw.replay.as_ref().map(replay_dto),
            strategy: None,
        };

        let json = serde_json::to_string(&reveal).unwrap();

        assert!(!json.contains("probability"), "{json}");
        assert!(!json.contains("strategy\":"), "{json}");
        assert!(!json.to_lowercase().contains("odds"), "{json}");
        assert!(!json.contains("0.75"), "{json}");
        // The replay exposes the simulation count but no solver node count.
        assert!(json.contains("\"turnsSimulated\":40"), "{json}");
        assert!(!json.contains("nodes"), "{json}");
        assert_eq!(reveal.source, "strategy");
    }

    // ── The preview draw ────────────────────────────────────────────────────

    fn preview_choice(index: usize) -> TeamPreviewCommand {
        TeamPreviewCommand {
            active_indices: vec![index],
            back_indices: vec![index + 1],
        }
    }

    fn preview_strategy(weights: &[f64]) -> Vec<PreviewChoiceProb> {
        weights
            .iter()
            .enumerate()
            .map(|(index, probability)| PreviewChoiceProb {
                choice: preview_choice(index),
                probability: *probability,
            })
            .collect()
    }

    fn preview_checkpoint(generation: u64, weights: &[f64]) -> AnalysisCheckpoint {
        AnalysisCheckpoint {
            p2_strategy: P2Strategy::Preview(preview_strategy(weights)),
            ..checkpoint(generation, 0)
        }
    }

    /// A preview strategy names bring-and-lead choices, which a battle turn
    /// cannot play. The battle draw must therefore read none of it and fall
    /// through to its uniform case.
    #[test]
    fn a_preview_checkpoint_supplies_no_battle_strategy() {
        let preview = preview_checkpoint(0, &[0.6, 0.4]);

        assert!(battle_rows(&preview.p2_strategy).is_none());
        assert!(preview_rows(&preview.p2_strategy).is_some());
    }

    /// The mirror rule. A battle strategy names slot commands, which the
    /// preview turn cannot play.
    #[test]
    fn a_battle_checkpoint_supplies_no_preview_strategy() {
        let battle = checkpoint(0, 3);

        assert!(preview_rows(&battle.p2_strategy).is_none());
        assert!(battle_rows(&battle.p2_strategy).is_some());
    }

    /// The preview draw must follow the mixed strategy, exactly as the battle
    /// draw does. A solver that leads with its second choice at 25% has to
    /// reach that rate at the table.
    #[test]
    fn the_preview_draw_follows_the_strategy_weights() {
        let strategy = preview_strategy(&[0.75, 0.25]);
        let mut rng = StdRng::seed_from_u64(3);
        let mut counts = [0u32; 2];

        for _ in 0..20_000 {
            counts[sample_preview(&strategy, &mut rng)
                .unwrap()
                .1
                .active_indices[0]] += 1;
        }

        let second = f64::from(counts[1]) / 20_000.0;
        assert!((second - 0.25).abs() < 0.02, "{counts:?}");

        // One seed must give one answer, so an operator can repeat a battle.
        let lead = |seed: u64| {
            let mut rng = StdRng::seed_from_u64(seed);
            sample_preview(&strategy, &mut rng)
                .unwrap()
                .1
                .active_indices
                .clone()
        };
        let first = lead(88);
        for _ in 0..20 {
            assert_eq!(lead(88), first);
        }

        // An empty strategy sends the caller to the uniform case.
        assert!(sample_preview(&[], &mut rng).is_none());
    }

    /// A fog-of-war session with an exact algorithm searches the true preview
    /// state, so its leads read P1's hidden team data. That answer must not
    /// control P2, and the preview position is no exemption from the rule.
    #[test]
    fn a_fogged_preview_answer_from_an_exact_search_cannot_control_p2() {
        for search in [
            BotSearchConfig::Exact(Default::default()),
            BotSearchConfig::Mcts(Default::default()),
        ] {
            assert!(!strategy_respects_fog(search, true));
        }

        let mut state = AnalysisState::default();
        let job = state.start();
        let mut fogged = preview_checkpoint(0, &[1.0]);
        fogged.strategy_is_playable =
            strategy_respects_fog(BotSearchConfig::Exact(Default::default()), true);
        state.accept(&job, Ok(fogged));

        // The checkpoint is current, and the gate still rejects it.
        assert!(state.current_checkpoint().is_some());
        assert_eq!(
            playable_checkpoint(&state).err(),
            Some("the strategy read data that the fog of war hides")
        );
    }

    /// A belief algorithm is the one case that may read the preview belief, so
    /// its answer controls P2 under the fog of war.
    #[test]
    fn a_fogged_preview_answer_from_a_belief_search_controls_p2() {
        let mut state = AnalysisState::default();
        let job = state.start();
        let mut playable = preview_checkpoint(0, &[1.0]);
        playable.strategy_is_playable =
            strategy_respects_fog(BotSearchConfig::Ismcts(Default::default()), true);
        state.accept(&job, Ok(playable));

        let checkpoint = playable_checkpoint(&state).expect("a belief answer is playable");
        assert!(preview_rows(&checkpoint.p2_strategy).is_some());
    }

    /// A stale checkpoint of either position must not reach a draw.
    #[test]
    fn the_gate_rejects_a_checkpoint_of_an_older_position() {
        let mut state = AnalysisState::default();
        let job = state.start();
        state.accept(&job, Ok(preview_checkpoint(0, &[1.0])));
        assert!(playable_checkpoint(&state).is_ok());

        state.invalidate();

        assert_eq!(
            playable_checkpoint(&state).err(),
            Some("no checkpoint covers this position")
        );
    }

    /// The collapse must keep every distinct line, so a real second reason
    /// still reaches the client.
    #[test]
    fn the_collapse_keeps_each_distinct_warning() {
        let checkpoint = partial_checkpoint(
            0.5,
            P2Strategy::Battle(Vec::new()),
            1,
            0,
            0,
            Duration::ZERO,
            &[
                solver::SolveWarning::ActionsTruncated {
                    player: Player::P1,
                    kept: 6,
                    total: 37,
                },
                solver::SolveWarning::ActionsTruncated {
                    player: Player::P2,
                    kept: 6,
                    total: 12,
                },
                solver::SolveWarning::DepthNotReached {
                    target: 3,
                    reached: 1,
                },
            ],
            &[],
        );

        assert_eq!(checkpoint.warnings.len(), 2, "{:?}", checkpoint.warnings);
        let joined = checkpoint.warnings.join("\n");
        assert!(joined.contains("action cap"), "{joined}");
        assert!(joined.contains("depth 1 of the 3"), "{joined}");
    }

    // ── The revealed strategy ───────────────────────────────────────────────
    // `botP2.revealStrategy` is the fog-of-war boundary of both battle
    // endpoints. These tests hold that boundary shut by default and check what
    // one open session reads.

    mod reveal {
        use super::*;
        use crate::bot::{BotProfile, BotProfileRequest};
        use crate::session::SessionConfig;
        use poke_rust::data::pokemon_move::PokemonMove;
        use poke_rust::data::species::Species;
        use poke_rust::simulator::{simulate_turn, team_preview_state_from_team_strings};
        use poke_rust::state::battle::{AttackCommand, BattleState};
        use poke_rust::state::dex_data::{
            MoveData, PokemonData, parse_move_dex, parse_pokemon_dex,
        };
        use std::sync::OnceLock;

        const TEAM_P1: &str = "Pikachu\nAbility: Static\nLevel: 50\n- Thunderbolt\n- Protect\n\n\
             Charizard\nAbility: Blaze\nLevel: 50\n- Flamethrower\n- Protect\n";
        const TEAM_P2: &str = "Snorlax\nAbility: Immunity\nLevel: 50\n- Body Slam\n- Protect\n\n\
             Gengar\nAbility: Levitate\nLevel: 50\n- Shadow Ball\n- Protect\n";

        static POKEMON_DEX: OnceLock<HashMap<Species, PokemonData>> = OnceLock::new();
        static MOVE_DEX: OnceLock<HashMap<PokemonMove, MoveData>> = OnceLock::new();
        static DEXES: OnceLock<Dexes> = OnceLock::new();

        fn pokemon_dex() -> &'static HashMap<Species, PokemonData> {
            POKEMON_DEX.get_or_init(|| parse_pokemon_dex("../pokemon_info/showdownDex.txt"))
        }

        fn move_dex() -> &'static HashMap<PokemonMove, MoveData> {
            MOVE_DEX.get_or_init(|| parse_move_dex("../pokemon_info/showdownMoves.txt"))
        }

        fn dexes() -> &'static Dexes {
            DEXES.get_or_init(|| Dexes {
                pokemon_dex: parse_pokemon_dex("../pokemon_info/showdownDex.txt"),
                move_dex: parse_move_dex("../pokemon_info/showdownMoves.txt"),
                ability_dex: poke_rust::state::dex_data::parse_ability_dex(
                    "../pokemon_info/showdownAbilities.txt",
                ),
                learnset_dex: poke_rust::state::dex_data::parse_learnset_dex(
                    "../pokemon_info/showdownLearnsets.txt",
                ),
            })
        }

        /// A singles preview of two two-Pokemon rosters.
        fn preview_state() -> TeamPreviewState {
            team_preview_state_from_team_strings(
                TEAM_P1,
                TEAM_P2,
                pokemon_dex(),
                move_dex(),
                1,
                2,
                true,
            )
        }

        /// The battle that follows the preview when both sides lead with their
        /// first Pokemon.
        fn battle_state() -> BattleState {
            let lead = PlayerCommand::TeamPreview(TeamPreviewCommand {
                active_indices: vec![0],
                back_indices: vec![1],
            });
            let branches = simulate_turn(
                &MatchState::TeamPreviewState(preview_state()),
                &lead,
                &lead,
                move_dex(),
                pokemon_dex(),
                false,
                1,
                None,
            );
            match &branches[0].0 {
                MatchState::BattleState(battle) => battle.clone(),
                other => panic!("a resolved preview gives a battle: {other:?}"),
            }
        }

        /// One battle strategy: attack, switch, then attack again.
        fn battle_strategy(weights: [f64; 3]) -> P2Strategy {
            P2Strategy::Battle(vec![
                JointActionProb {
                    commands: vec![BattleCommand::Attack(AttackCommand {
                        move_slot: 0,
                        target: None,
                        terastallize: false,
                        mega_evolve: false,
                    })],
                    probability: weights[0],
                },
                JointActionProb {
                    commands: vec![BattleCommand::Switch(SwitchCommand { party_index: 1 })],
                    probability: weights[1],
                },
                JointActionProb {
                    commands: vec![BattleCommand::Attack(AttackCommand {
                        move_slot: 1,
                        target: None,
                        terastallize: false,
                        mega_evolve: false,
                    })],
                    probability: weights[2],
                },
            ])
        }

        /// A session at `state` whose profile reveals the strategy or hides it.
        fn session(state: MatchState, reveal: bool) -> BattleSession {
            let request = BotProfileRequest {
                algorithm: Some("doubleOracle".to_string()),
                reveal_strategy: Some(reveal),
                ..BotProfileRequest::default()
            };
            let profile: BotProfile = crate::bot::resolve("botP2", &request).unwrap();
            BattleSession {
                state,
                config: SessionConfig {
                    active_per_side: 1,
                    brought_per_side: 2,
                    consider_crit: false,
                    damage_rolls: 1,
                    information_mode:
                        poke_rust::information::unknowns::InformationMode::PerfectInformation,
                },
                log_p1: Vec::new(),
                log_p2: Vec::new(),
                belief_p1: None,
                belief_p2: None,
                inference_config: None,
                bot_p2: Some(profile),
                analysis: AnalysisState::default(),
            }
        }

        /// Stores one complete checkpoint of the current position.
        fn store(session: &mut BattleSession, strategy: P2Strategy) {
            let job = session.analysis.start();
            let stored = AnalysisCheckpoint {
                p2_strategy: strategy,
                ..checkpoint(session.analysis.generation(), 1)
            };
            session.analysis.accept(&job, Ok(stored));
        }

        /// Every bot session sends its current strategy.
        #[test]
        fn a_bot_session_sends_its_strategy_row() {
            let mut hidden = session(MatchState::BattleState(battle_state()), false);
            store(&mut hidden, battle_strategy([0.5, 0.3, 0.2]));

            let json = serde_json::to_string(&progress_dto(&hidden)).unwrap();

            assert!(json.contains("p2Strategy"), "{json}");
            assert!(!json.to_lowercase().contains("thunderbolt"), "{json}");
            assert!(json.contains("elapsedMs"), "{json}");
        }

        /// A hotseat session holds no profile at all, so the gate stays shut
        /// however the request was written.
        #[test]
        fn a_hotseat_session_reveals_nothing() {
            let mut hotseat = session(MatchState::BattleState(battle_state()), true);
            hotseat.bot_p2 = None;
            store(&mut hotseat, battle_strategy([1.0, 0.0, 0.0]));

            assert!(!reveals_strategy(&hotseat));
            let json = serde_json::to_string(&progress_dto(&hotseat)).unwrap();
            assert!(!json.contains("p2Strategy"), "{json}");
        }

        /// The open session reads the strategy of the position it is playing.
        #[test]
        fn a_revealed_session_sends_the_rows_of_the_current_position() {
            let mut open = session(MatchState::BattleState(battle_state()), true);
            store(&mut open, battle_strategy([0.5, 0.3, 0.2]));

            let view = progress_dto(&open);
            let strategy = view
                .checkpoint
                .as_ref()
                .and_then(|row| row.p2_strategy.as_ref())
                .expect("an open session reads the strategy");

            assert_eq!(strategy.position, "battle");
            assert_eq!(strategy.total, 3);
            assert_eq!(strategy.rows.len(), 3);
            // The checkpoint names no draw.
            assert_eq!(strategy.drawn_index, None);
            // Highest rate first.
            let rates: Vec<f64> = strategy.rows.iter().map(|row| row.probability).collect();
            assert_eq!(rates, vec![0.5, 0.3, 0.2]);
            // Each row renders one command of one slot against the field.
            assert_eq!(strategy.rows[0].commands.len(), 1);
            assert!(strategy.rows[0].commands[0].description.len() > 1);
        }

        /// A stale answer names actions of an older position, so rendering it
        /// against the current field would describe the wrong Pokemon.
        #[test]
        fn a_stale_checkpoint_sends_no_strategy_row() {
            let mut open = session(MatchState::BattleState(battle_state()), true);
            store(&mut open, battle_strategy([0.5, 0.3, 0.2]));
            open.analysis.invalidate();

            let view = progress_dto(&open);

            assert!(view.checkpoint.as_ref().unwrap().stale);
            assert!(view.checkpoint.unwrap().p2_strategy.is_none());
        }

        /// A row that the strategy never plays tells the reader nothing, so the
        /// list drops it and the total counts only the rows that remain.
        #[test]
        fn the_rows_drop_a_zero_rate() {
            let state = MatchState::BattleState(battle_state());
            let strategy = strategy_dto(&state, &battle_strategy([0.7, 0.0, 0.3]), None).unwrap();

            assert_eq!(strategy.total, 2);
            let rates: Vec<f64> = strategy.rows.iter().map(|row| row.probability).collect();
            assert_eq!(rates, vec![0.7, 0.3]);
        }

        /// The response includes every positive row and identifies a low-rate draw.
        #[test]
        fn the_response_keeps_the_full_strategy_and_the_drawn_row() {
            let state = MatchState::BattleState(battle_state());
            let row_count = 57;
            let rows = vec![
                JointActionProb {
                    commands: vec![BattleCommand::Switch(SwitchCommand { party_index: 1 })],
                    probability: 0.01,
                };
                row_count
            ];

            let strategy =
                strategy_dto(&state, &P2Strategy::Battle(rows), Some(row_count - 1)).unwrap();

            assert_eq!(strategy.rows.len(), row_count);
            assert_eq!(strategy.total, row_count);
            assert_eq!(strategy.drawn_index, Some(row_count - 1));
        }

        /// The reveal must name the row that supplied the drawn command, after
        /// the sort moved it.
        #[test]
        fn the_drawn_index_names_the_drawn_row() {
            let state = MatchState::BattleState(battle_state());
            // Source order 0, 1, 2 has rates 0.2, 0.5, 0.3, so the sort gives
            // 1, 2, 0. Source row 0 therefore lands last.
            let strategy =
                strategy_dto(&state, &battle_strategy([0.2, 0.5, 0.3]), Some(0)).unwrap();

            assert_eq!(strategy.drawn_index, Some(2));
            assert_eq!(strategy.rows[2].probability, 0.2);

            // A drawn row that the zero-rate rule removed has no place in the
            // list, and the block says so rather than naming another row.
            let dropped = strategy_dto(&state, &battle_strategy([0.6, 0.0, 0.4]), Some(1)).unwrap();
            assert_eq!(dropped.drawn_index, None);
        }

        /// A preview block names the leads and the back picks of Player 2. That
        /// is hidden data, so only the open session reads it.
        #[test]
        fn a_preview_block_names_the_leads_and_the_back_picks() {
            let state = MatchState::TeamPreviewState(preview_state());
            let strategy = P2Strategy::Preview(vec![PreviewChoiceProb {
                choice: TeamPreviewCommand {
                    active_indices: vec![1],
                    back_indices: vec![0],
                },
                probability: 1.0,
            }]);

            let rendered = strategy_dto(&state, &strategy, Some(0)).unwrap();

            assert_eq!(rendered.position, "teamPreview");
            assert_eq!(rendered.drawn_index, Some(0));
            let choice = rendered.rows[0].preview.as_ref().unwrap();
            assert_eq!(choice.leads, vec!["Gengar".to_string()]);
            assert_eq!(choice.back, vec!["Snorlax".to_string()]);
        }

        /// A battle strategy cannot render against a preview roster, and a
        /// preview strategy cannot render against a field. The draw applies the
        /// same rule.
        #[test]
        fn a_strategy_of_the_other_position_renders_nothing() {
            let battle = MatchState::BattleState(battle_state());
            let preview = MatchState::TeamPreviewState(preview_state());
            let preview_strategy = P2Strategy::Preview(Vec::new());

            assert!(strategy_dto(&battle, &preview_strategy, None).is_none());
            assert!(strategy_dto(&preview, &battle_strategy([1.0, 0.0, 0.0]), None).is_none());
        }

        /// Every bot draw can carry the strategy that supplied it.
        #[test]
        fn each_bot_reveal_carries_its_strategy() {
            let strategy = battle_strategy([0.5, 0.3, 0.2]);
            let mut hidden = session(MatchState::BattleState(battle_state()), false);
            store(&mut hidden, strategy.clone());
            let mut open = session(MatchState::BattleState(battle_state()), true);
            store(&mut open, strategy);

            let stored = |session: &BattleSession| {
                let checkpoint = session.analysis.current_checkpoint().unwrap();
                revealed_strategy(session, checkpoint, 1)
            };

            assert_eq!(stored(&hidden).unwrap().drawn_index, Some(1));
            let revealed = stored(&open).expect("an open session reveals the draw");
            assert_eq!(revealed.drawn_index, Some(1));
        }

        #[test]
        fn a_replacement_draw_skips_an_invalid_strategy_row() {
            let mut battle = battle_state();
            battle.p2_active_mons[0].hp = 0;
            battle.p2_active_mons[0].fainted = true;
            battle.turn_started = true;
            battle.turn_ended = true;
            let session = session(MatchState::BattleState(battle), false);
            let rows = vec![
                JointActionProb {
                    commands: vec![BattleCommand::Pass],
                    probability: 0.9,
                },
                JointActionProb {
                    commands: vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })],
                    probability: 0.1,
                },
            ];
            let mut rng = StdRng::seed_from_u64(7);

            let (index, command) = sample_legal_strategy(&session, dexes(), &rows, &mut rng)
                .expect("the legal replacement remains");

            assert_eq!(index, 1);
            assert!(matches!(
                command,
                PlayerCommand::Battle(commands)
                    if matches!(commands.as_slice(), [BattleCommand::Switch(command)] if command.party_index == 0)
            ));
        }

        #[test]
        fn one_replacement_choice_skips_the_search() {
            let mut battle = battle_state();
            battle.p2_active_mons[0].hp = 0;
            battle.p2_active_mons[0].fainted = true;
            battle.turn_started = true;
            battle.turn_ended = true;
            let session = session(MatchState::BattleState(battle), false);
            let replay = AnalysisReplay {
                generation: 0,
                turn_number: 1,
                search_seed: 7,
                ..AnalysisReplay::default()
            };

            let checkpoint = forced_checkpoint(&session, dexes(), 7, replay)
                .expect("one replacement produces an immediate strategy");

            let P2Strategy::Battle(rows) = checkpoint.p2_strategy else {
                panic!("the replacement must be a battle strategy");
            };
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].probability, 1.0);
            assert!(matches!(rows[0].commands[0], BattleCommand::Switch(_)));
            assert_eq!(checkpoint.turns_simulated, 0);
        }
    }
}
