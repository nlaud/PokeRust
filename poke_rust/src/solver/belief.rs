//! Weighted particles over the hidden data of one battle position.
//!
//! A fog-of-war search cannot read an [`UnknownBattleState`]. The engine needs a
//! concrete world. [`ParticleBelief`] holds a set of concrete worlds and one
//! weight for each world. The weights total one, so a draw from the set is a
//! draw from the belief.
//!
//! # The observation model
//!
//! A player never sees the hidden data of the other player. A player sees the
//! event stream of each turn, after
//! [`mask_events_for`](crate::information::information::mask_events_for) removes
//! the private parts. [`ObservationKey`] hashes that stream, the player's own
//! commands, and the player's private Pokemon state.
//!
//! Two hidden worlds that gave one player the same key are one information set
//! of that player. A search must group them, because the player cannot tell them
//! apart. [`ismcts`](super::ismcts) keys its trees by this type for that reason.
//!
//! # Weights
//!
//! [`ParticleBelief::from_belief`] samples each world from the determinizer.
//! Each sampled world has the same empirical weight. The sample frequency
//! already represents the determinizer distribution.
//!
//! # Degeneracy
//!
//! A posterior update multiplies each weight by a likelihood. A few particles
//! then hold most of the weight, and the rest add nothing to an estimate.
//!
//! [`ParticleBelief::effective_sample_size`] measures that loss. It returns the
//! particle count of an equally weighted set with the same variance.
//! [`ParticleBelief::resample_systematic`] rebuilds the set at equal weights.
//! It copies a heavy particle many times and drops a light particle.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::data::pokemon_move::PokemonMove;
use crate::data::species::Species;
use crate::information::determinize::{
    DeterminizeConfig, DeterminizeError, DeterminizeWarning, determinize_seeded,
};
use crate::information::information::InformationEvent;
use crate::information::unknowns::UnknownBattleState;
use crate::meta::MetaDex;
use crate::simulator::generative::{TransitionConfig, sample_transition};
use crate::simulator::with_sample_rng;
use crate::state::battle::{
    BattleCommand, BattleState, FieldSlot, MatchState, Player, PlayerCommand,
};
use crate::state::dex_data::{MoveData, PokemonData};

use rand::Rng;

// ── Observation keys ────────────────────────────────────────────────────────

/// What one player knows, as a running hash.
///
/// The key holds the player's private Pokemon state, its commands, and its masked
/// events. It never holds the hidden data of the other player. It also never
/// holds a complete `MatchState` hash.
///
/// A hash collision merges two information sets. Each node reads its own action
/// list from the current world, so a merge changes a value and can never index
/// outside a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ObservationKey(u64);

impl ObservationKey {
    /// The key of a player that has seen nothing yet.
    pub const ROOT: ObservationKey = ObservationKey(0);

    /// The key of one masked event stream, with no history before it.
    pub fn of(events: &[InformationEvent]) -> Self {
        ObservationKey::ROOT.extend(&[], events)
    }

    /// Make the initial key for one player in one world.
    ///
    /// A player knows all data for its own Pokemon. Hidden worlds with different
    /// own data are different information sets for that player.
    pub fn for_player(battle: &BattleState, player: Player) -> Self {
        let mut hasher = DefaultHasher::new();
        match player {
            Player::P1 => {
                battle.p1_active_mons.hash(&mut hasher);
                battle.p1_back_mons.hash(&mut hasher);
            }
            Player::P2 => {
                battle.p2_active_mons.hash(&mut hasher);
                battle.p2_back_mons.hash(&mut hasher);
            }
        }
        ObservationKey(hasher.finish())
    }

    /// Add one turn to the history of a player.
    ///
    /// `commands` is what the player itself submitted. A player always knows its
    /// own command, so two turns with different commands are different
    /// information sets even when the events agree.
    ///
    /// `events` is the stream that
    /// [`mask_events_for`](crate::information::information::mask_events_for)
    /// built for the same player.
    pub fn extend(self, commands: &[BattleCommand], events: &[InformationEvent]) -> Self {
        let mut hasher = DefaultHasher::new();
        self.0.hash(&mut hasher);
        JointActionKey::new(commands).hash(&mut hasher);
        events.hash(&mut hasher);
        ObservationKey(hasher.finish())
    }

    /// The hash itself. Tests and node keys read it.
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// One joint action, in a form that a map can key.
///
/// [`BattleCommand`] implements neither `Eq` nor `Hash`, so a registry cannot
/// use it directly. This type copies every field that separates two commands, so
/// two joint actions share a key exactly when the commands are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JointActionKey(Vec<CommandKey>);

impl JointActionKey {
    pub fn new(commands: &[BattleCommand]) -> Self {
        JointActionKey(commands.iter().map(CommandKey::new).collect())
    }
}

/// One slot command, in a form that a map can key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CommandKey {
    Attack {
        move_slot: usize,
        target: Option<FieldSlot>,
        terastallize: bool,
        mega_evolve: bool,
    },
    Switch(usize),
    Struggle(Option<FieldSlot>),
    Pass,
}

impl CommandKey {
    fn new(command: &BattleCommand) -> Self {
        match command {
            BattleCommand::Attack(attack) => CommandKey::Attack {
                move_slot: attack.move_slot,
                target: attack.target,
                terastallize: attack.terastallize,
                mega_evolve: attack.mega_evolve,
            },
            BattleCommand::Switch(switch) => CommandKey::Switch(switch.party_index),
            BattleCommand::Struggle { target } => CommandKey::Struggle(*target),
            BattleCommand::Pass => CommandKey::Pass,
        }
    }
}

// ── The particle set ────────────────────────────────────────────────────────

/// One concrete world and how much of the belief it carries.
#[derive(Debug, Clone)]
pub struct Particle {
    /// The world itself. A posterior update can advance it to a finished battle.
    pub state: MatchState,
    /// The share of the belief, from 0 through 1.
    pub weight: f64,
}

/// A weighted set of worlds that one belief permits.
#[derive(Debug, Clone)]
pub struct ParticleBelief {
    particles: Vec<Particle>,
    warnings: Vec<DeterminizeWarning>,
}

/// Why a particle operation failed.
#[derive(Debug, Clone, PartialEq)]
pub enum BeliefError {
    /// The caller asked for zero particles, or gave an empty set.
    NoParticles,
    /// The determinizer could not draw one world.
    Draw {
        world: usize,
        error: DeterminizeError,
    },
    /// No particle produced the observation, so the posterior is empty.
    ///
    /// A larger `samples` value finds a rare observation more often. A belief
    /// that cannot explain the observation at all needs a new draw.
    NoMatch { samples: usize },
}

impl fmt::Display for BeliefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BeliefError::NoParticles => write!(f, "a belief needs at least one particle"),
            BeliefError::Draw { world, error } => {
                write!(f, "world {world}: the determinizer failed: {error}")
            }
            BeliefError::NoMatch { samples } => write!(
                f,
                "no particle produced the observation in {samples} samples"
            ),
        }
    }
}

impl std::error::Error for BeliefError {}

/// What one posterior update did to the set.
#[derive(Debug, Clone, PartialEq)]
pub struct PosteriorUpdate {
    /// Particles that produced the observation at least one time.
    pub matched: usize,
    /// Particles that the update removed.
    pub dropped: usize,
    /// The effective sample size after the update.
    pub effective_sample_size: f64,
    /// Whether the update rebuilt the set at equal weights.
    pub resampled: bool,
}

impl ParticleBelief {
    /// Draw `count` worlds from `belief`.
    ///
    /// World `w` uses seed `seed + w`. Thus, the same inputs always give the same
    /// set. Each draw has the same empirical weight because the determinizer
    /// already samples from its target distribution.
    ///
    /// The determinizer copies the side of
    /// [`DeterminizeConfig::observer`] and samples the other side. The observer
    /// already knows its own team, so only the hidden side changes between
    /// worlds.
    pub fn from_belief(
        seed: u64,
        belief: &UnknownBattleState,
        meta_dex: &MetaDex,
        pokemon_dex: &HashMap<Species, PokemonData>,
        move_dex: &HashMap<PokemonMove, MoveData>,
        count: usize,
        config: &DeterminizeConfig,
    ) -> Result<Self, BeliefError> {
        if count == 0 {
            return Err(BeliefError::NoParticles);
        }
        let mut particles = Vec::with_capacity(count);
        let mut warnings: Vec<DeterminizeWarning> = Vec::new();
        for world in 0..count {
            let drawn = determinize_seeded(
                seed.wrapping_add(world as u64),
                belief,
                meta_dex,
                pokemon_dex,
                move_dex,
                config,
            )
            .map_err(|error| BeliefError::Draw { world, error })?;
            for warning in &drawn.warnings {
                if !warnings.contains(warning) {
                    warnings.push(warning.clone());
                }
            }
            particles.push(Particle {
                state: MatchState::BattleState(drawn.state),
                weight: 1.0,
            });
        }
        let mut set = ParticleBelief {
            particles,
            warnings,
        };
        set.normalize();
        Ok(set)
    }

    /// Build a set from worlds that a caller already holds.
    ///
    /// The weights normalize, so a caller can pass raw likelihoods. A test that
    /// hides nothing passes one world with any positive weight.
    pub fn from_particles(particles: Vec<Particle>) -> Result<Self, BeliefError> {
        if particles.is_empty() {
            return Err(BeliefError::NoParticles);
        }
        let mut set = ParticleBelief {
            particles,
            warnings: Vec::new(),
        };
        set.normalize();
        Ok(set)
    }

    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    pub fn len(&self) -> usize {
        self.particles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// What the determinizer reported while it drew the worlds.
    /// Each distinct warning appears one time.
    pub fn warnings(&self) -> &[DeterminizeWarning] {
        &self.warnings
    }

    /// Scale every weight so that the total is one.
    ///
    /// A set whose weights total zero becomes uniform. The alternative is a
    /// division by zero, and a uniform set keeps every world that the belief
    /// permitted.
    pub fn normalize(&mut self) {
        for particle in &mut self.particles {
            if !particle.weight.is_finite() || particle.weight < 0.0 {
                particle.weight = 0.0;
            }
        }
        let total: f64 = self.particles.iter().map(|particle| particle.weight).sum();
        if !total.is_finite() || total <= 0.0 {
            let uniform = 1.0 / self.particles.len() as f64;
            for particle in &mut self.particles {
                particle.weight = uniform;
            }
            return;
        }
        for particle in &mut self.particles {
            particle.weight /= total;
        }
    }

    /// The particle count of an equally weighted set with the same variance.
    ///
    /// The result is the inverse of the sum of the squared weights. An equally
    /// weighted set returns its own count. A set whose weight sits on one
    /// particle returns one.
    pub fn effective_sample_size(&self) -> f64 {
        let squares: f64 = self
            .particles
            .iter()
            .map(|particle| particle.weight * particle.weight)
            .sum();
        if squares <= 0.0 {
            return 0.0;
        }
        1.0 / squares
    }

    /// Rebuild the set at equal weights, with the same particle count.
    ///
    /// Systematic resampling walks one comb of evenly spaced points over the
    /// cumulative weights. One uniform draw places the comb. A particle of
    /// weight `w` therefore gets `floor(n * w)` copies or one more, which keeps
    /// the resampling noise below the noise of independent draws.
    ///
    /// The draw uses the ambient sample generator, so a seeded caller keeps one
    /// result.
    pub fn resample_systematic(&mut self) {
        let count = self.particles.len();
        if count == 0 {
            return;
        }
        self.normalize();
        let step = 1.0 / count as f64;
        let start = with_sample_rng(|rng| rng.gen_range(0.0..1.0)) * step;

        let mut drawn: Vec<Particle> = Vec::with_capacity(count);
        let mut source = 0usize;
        let mut cumulative = self.particles[0].weight;
        for member in 0..count {
            let target = start + member as f64 * step;
            while cumulative < target && source + 1 < count {
                source += 1;
                cumulative += self.particles[source].weight;
            }
            drawn.push(Particle {
                state: self.particles[source].state.clone(),
                weight: step,
            });
        }
        self.particles = drawn;
    }

    /// Resample when the effective sample size falls below a share of the count.
    ///
    /// `threshold` is that share, from 0 through 1. A threshold of zero never
    /// resamples. Returns whether the set changed.
    pub fn resample_if_degenerate(&mut self, threshold: f64) -> bool {
        let floor = threshold.clamp(0.0, 1.0) * self.particles.len() as f64;
        if self.effective_sample_size() >= floor || self.particles.is_empty() {
            return false;
        }
        self.resample_systematic();
        true
    }

    /// Draw one particle in proportion to its weight.
    ///
    /// The draw uses the ambient sample generator, so a seeded caller keeps one
    /// result. An empty set returns `None`.
    pub fn draw(&self) -> Option<&Particle> {
        if self.particles.is_empty() {
            return None;
        }
        let roll: f64 = with_sample_rng(|rng| rng.gen_range(0.0..1.0));
        let mut accumulated = 0.0;
        for particle in &self.particles {
            accumulated += particle.weight;
            if roll < accumulated {
                return Some(particle);
            }
        }
        // Rounding left the roll above the total. The last particle covers it.
        self.particles.last()
    }

    /// Apply one turn and one observation to the set.
    ///
    /// Both players submit a command. The turn resolves `samples` times for each
    /// particle. The method counts the resolutions whose masked stream matches
    /// `observed`, and it multiplies the weight of the particle by that share.
    /// The share is an unbiased estimate of the chance that the world produces
    /// the observation.
    ///
    /// The state of a surviving particle becomes one of its matching successors,
    /// drawn uniformly. The set is therefore a posterior over the successor
    /// worlds, not over the earlier worlds.
    ///
    /// The method removes every particle of weight zero, and it then
    /// normalizes. An empty posterior returns [`BeliefError::NoMatch`], and the
    /// set keeps its earlier contents.
    ///
    /// `resample_threshold` runs [`ParticleBelief::resample_if_degenerate`] at
    /// the end, so the caller does not have to.
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_observation(
        &mut self,
        observer: Player,
        observed: &[InformationEvent],
        p1_cmd: &PlayerCommand,
        p2_cmd: &PlayerCommand,
        pokemon_dex: &HashMap<Species, PokemonData>,
        move_dex: &HashMap<PokemonMove, MoveData>,
        config: TransitionConfig,
        samples: usize,
        resample_threshold: f64,
    ) -> Result<PosteriorUpdate, BeliefError> {
        let samples = samples.max(1);
        if self.particles.is_empty() {
            return Err(BeliefError::NoParticles);
        }
        // The turn must report its events. A caller that left the flag false
        // would compare against nothing.
        let config = TransitionConfig {
            observe: true,
            ..config
        };
        let target = ObservationKey::of(observed);

        let mut posterior: Vec<Particle> = Vec::with_capacity(self.particles.len());
        let mut matched = 0usize;
        for particle in &self.particles {
            let mut successors: Vec<MatchState> = Vec::new();
            for _ in 0..samples {
                let sample = sample_transition(
                    &particle.state,
                    p1_cmd,
                    p2_cmd,
                    move_dex,
                    pokemon_dex,
                    config,
                );
                let views = sample
                    .observations
                    .as_ref()
                    .expect("the config sets the observe flag");
                let seen = match observer {
                    Player::P1 => &views.p1,
                    Player::P2 => &views.p2,
                };
                if ObservationKey::of(seen) == target {
                    successors.push(sample.state);
                }
            }
            if successors.is_empty() {
                continue;
            }
            matched += 1;
            let likelihood = successors.len() as f64 / samples as f64;
            let chosen = with_sample_rng(|rng| rng.gen_range(0..successors.len()));
            posterior.push(Particle {
                state: successors.swap_remove(chosen),
                weight: particle.weight * likelihood,
            });
        }

        if posterior.is_empty() {
            return Err(BeliefError::NoMatch { samples });
        }
        let dropped = self.particles.len() - posterior.len();
        self.particles = posterior;
        self.normalize();
        let resampled = self.resample_if_degenerate(resample_threshold);
        Ok(PosteriorUpdate {
            matched,
            dropped,
            effective_sample_size: self.effective_sample_size(),
            resampled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::item::Item;
    use crate::simulator::scoped_sample_rng;
    use crate::state::battle::{AttackCommand, SwitchCommand};
    use crate::state::pokemon::{Nature, PokemonState, build_pokemon_state};
    use crate::tests::simuilator_test_helpers::{battle_state_from_lists, move_dex, pokemon_dex};

    fn pikachu() -> PokemonState {
        build_pokemon_state(
            Species::Pikachu,
            pokemon_dex(),
            move_dex(),
            Some(50),
            Some([Some(PokemonMove::Tackle), None, None, None]),
            None,
            None,
            Some(Nature::Hardy),
            Some(Item::None),
            None,
            Some([0; 6]),
            Some([31; 6]),
            true,
        )
    }

    /// One world, tagged with `turn`. The particle tests read the weights alone,
    /// so the tag is the only part of the world that they compare.
    fn world(turn: u16) -> MatchState {
        let mut battle = battle_state_from_lists(vec![pikachu()], vec![], vec![pikachu()], vec![]);
        battle.turn_number = turn;
        MatchState::BattleState(battle)
    }

    /// The tag of a world that [`world`] built.
    fn tag_of(state: &MatchState) -> u16 {
        match state {
            MatchState::BattleState(battle) => battle.turn_number,
            _ => panic!("the fixture builds battle states"),
        }
    }

    fn belief_of(weights: &[f64]) -> ParticleBelief {
        ParticleBelief::from_particles(
            weights
                .iter()
                .enumerate()
                .map(|(index, &weight)| Particle {
                    state: world(index as u16),
                    weight,
                })
                .collect(),
        )
        .expect("the list is not empty")
    }

    #[test]
    fn normalization_gives_a_total_of_one() {
        let belief = belief_of(&[2.0, 3.0, 5.0]);
        let total: f64 = belief.particles().iter().map(|p| p.weight).sum();
        assert!((total - 1.0).abs() < 1e-12, "the total is {total}");
        assert!((belief.particles()[0].weight - 0.2).abs() < 1e-12);
    }

    /// A set whose weights are all zero must stay usable. A division by zero
    /// would give every particle a weight that is not a number.
    #[test]
    fn a_zero_total_gives_uniform_weights() {
        let belief = belief_of(&[0.0, 0.0, 0.0, 0.0]);
        for particle in belief.particles() {
            assert!((particle.weight - 0.25).abs() < 1e-12, "{particle:?}");
        }
    }

    /// Invalid likelihoods must not produce negative or non-finite weights.
    #[test]
    fn normalization_removes_invalid_weights() {
        let belief = belief_of(&[-1.0, f64::NAN, 2.0]);
        assert_eq!(belief.particles()[0].weight, 0.0);
        assert_eq!(belief.particles()[1].weight, 0.0);
        assert_eq!(belief.particles()[2].weight, 1.0);
        assert!((belief.effective_sample_size() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_effective_sample_size_falls_as_one_weight_grows() {
        let even = belief_of(&[1.0, 1.0, 1.0, 1.0]);
        assert!((even.effective_sample_size() - 4.0).abs() < 1e-9);

        let skewed = belief_of(&[97.0, 1.0, 1.0, 1.0]);
        assert!(
            skewed.effective_sample_size() < 1.1,
            "the skewed set reports {}",
            skewed.effective_sample_size()
        );
        assert!(skewed.effective_sample_size() < even.effective_sample_size());
    }

    #[test]
    fn resampling_keeps_the_count_and_flattens_the_weights() {
        let _guard = scoped_sample_rng(7);
        let mut belief = belief_of(&[90.0, 5.0, 3.0, 2.0]);
        belief.resample_systematic();

        assert_eq!(belief.len(), 4);
        for particle in belief.particles() {
            assert!((particle.weight - 0.25).abs() < 1e-12, "{particle:?}");
        }
        assert!((belief.effective_sample_size() - 4.0).abs() < 1e-9);
    }

    /// A comb of two points over a weight of 0.9 must land on that particle one
    /// time or two times, whatever the placing draw is.
    #[test]
    fn systematic_resampling_copies_a_heavy_particle() {
        for seed in 0..8u64 {
            let _guard = scoped_sample_rng(seed);
            let mut belief = belief_of(&[0.9, 0.1]);
            belief.resample_systematic();
            let heavy = belief
                .particles()
                .iter()
                .filter(|particle| tag_of(&particle.state) == 0)
                .count();
            assert!((1..=2).contains(&heavy), "seed {seed} copied {heavy}");
        }
    }

    #[test]
    fn a_threshold_of_zero_never_resamples() {
        let _guard = scoped_sample_rng(3);
        let mut belief = belief_of(&[99.0, 1.0]);
        assert!(!belief.resample_if_degenerate(0.0));
        assert!(belief.particles()[0].weight > 0.9);
        assert!(belief.resample_if_degenerate(1.0));
    }

    /// The draw must follow the weights. A particle of weight zero must never
    /// come back.
    #[test]
    fn a_draw_follows_the_weights() {
        let _guard = scoped_sample_rng(11);
        let belief = belief_of(&[0.0, 1.0]);
        for _ in 0..32 {
            let drawn = belief.draw().expect("the set is not empty");
            assert_eq!(tag_of(&drawn.state), 1);
        }
    }

    #[test]
    fn an_empty_particle_list_is_an_error() {
        assert_eq!(
            ParticleBelief::from_particles(Vec::new()).unwrap_err(),
            BeliefError::NoParticles
        );
    }

    /// The key must separate two different commands and must join two equal
    /// ones. A registry and a node key both depend on that.
    #[test]
    fn a_joint_action_key_separates_different_commands() {
        let attack = |slot: usize, tera: bool| {
            vec![BattleCommand::Attack(AttackCommand {
                move_slot: slot,
                target: None,
                terastallize: tera,
                mega_evolve: false,
            })]
        };
        let switch = vec![BattleCommand::Switch(SwitchCommand { party_index: 0 })];

        assert_eq!(
            JointActionKey::new(&attack(0, false)),
            JointActionKey::new(&attack(0, false))
        );
        assert_ne!(
            JointActionKey::new(&attack(0, false)),
            JointActionKey::new(&attack(1, false))
        );
        assert_ne!(
            JointActionKey::new(&attack(0, false)),
            JointActionKey::new(&attack(0, true))
        );
        assert_ne!(
            JointActionKey::new(&attack(0, false)),
            JointActionKey::new(&switch)
        );
    }

    /// The history must depend on the command and on the events. Two players
    /// that saw the same events after different commands are two information
    /// sets.
    #[test]
    fn an_observation_key_holds_the_command_and_the_events() {
        let pass = vec![BattleCommand::Pass];
        let switch = vec![BattleCommand::Switch(SwitchCommand { party_index: 1 })];

        assert_eq!(
            ObservationKey::ROOT.extend(&pass, &[]),
            ObservationKey::ROOT.extend(&pass, &[])
        );
        assert_ne!(
            ObservationKey::ROOT.extend(&pass, &[]),
            ObservationKey::ROOT.extend(&switch, &[])
        );
        // A history is a chain, so one turn of it must not equal two turns.
        let first = ObservationKey::ROOT.extend(&pass, &[]);
        assert_ne!(first, first.extend(&pass, &[]));
    }

    /// Each player knows its own stats. Only that player's key must change when
    /// its stats change between two hidden worlds.
    #[test]
    fn an_initial_key_holds_the_players_own_state() {
        let MatchState::BattleState(first) = world(1) else {
            panic!("the fixture builds a battle state");
        };
        let mut second = first.clone();
        second.p2_active_mons[0].stats[5] += 1;

        assert_eq!(
            ObservationKey::for_player(&first, Player::P1),
            ObservationKey::for_player(&second, Player::P1)
        );
        assert_ne!(
            ObservationKey::for_player(&first, Player::P2),
            ObservationKey::for_player(&second, Player::P2)
        );
    }
}
