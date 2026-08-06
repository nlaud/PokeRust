//! The information-set plumbing that every fog-of-war search shares.
//!
//! [`ismcts`](super::ismcts) and [`mccfr`](super::mccfr) both hold one tree for
//! each player. A node of that tree is one information set, not one position.
//! This module holds the key, the node, and the root report.
//!
//! # The key
//!
//! [`InfoKey`] holds the [`ObservationKey`] of one player, the search depth, and
//! the forced-chain counter. The key never holds a complete `MatchState` hash.
//! Two hidden worlds share a node when the player has the same private state,
//! commands, and events.
//!
//! # The action registry
//!
//! One node covers many worlds, and two worlds can offer different actions. An
//! [`InfoNode`] therefore holds a registry that maps one joint action to one
//! learner index. A visit plays only the registered actions that its own world
//! permits, so the learner works as a subset-armed bandit.
//!
//! # The root report
//!
//! [`root_strategy`] mixes the root information sets of one player. It uses the
//! sample weight in each average. Read each search for the meaning of a mixture.

use std::collections::HashMap;

use crate::state::battle::BattleCommand;

use super::JointActionProb;
use super::actions::JointActions;
use super::belief::{JointActionKey, ObservationKey};
use super::mcts::Learner;
use super::search::strategy_of;

/// One information set: what a player saw, the depth, and the chain counter.
pub(super) type InfoKey = (ObservationKey, u8, u8);

/// One information set of one player.
pub(super) struct InfoNode {
    pub(super) learner: Learner,
    /// The learner index of each registered joint action.
    index_of: HashMap<JointActionKey, usize>,
    /// The joint action of each learner index, for the report.
    commands: Vec<Vec<BattleCommand>>,
}

impl InfoNode {
    pub(super) fn new() -> Self {
        InfoNode {
            learner: Learner::new(0),
            index_of: HashMap::new(),
            commands: Vec::new(),
        }
    }

    /// Register `actions`, and return the learner index of each one in order.
    ///
    /// An action that a world already offered keeps its index, so the learner
    /// keeps what it learned about it. A new action gets a new index and a score
    /// of zero.
    pub(super) fn register(&mut self, actions: &[Vec<BattleCommand>]) -> Vec<usize> {
        let mut indexes = Vec::with_capacity(actions.len());
        for commands in actions {
            let key = JointActionKey::new(commands);
            let index = match self.index_of.get(&key) {
                Some(&index) => index,
                None => {
                    let index = self.commands.len();
                    self.index_of.insert(key, index);
                    self.commands.push(commands.clone());
                    index
                }
            };
            indexes.push(index);
        }
        self.learner.grow_to(self.commands.len());
        indexes
    }
}

/// The average strategy over every root information set of one player.
///
/// Each set contributes its average strategy, weighted by the samples in its
/// average. A player with one root set therefore reports that set alone.
///
/// An empty vector means that no iteration reached a root node. Only a belief of
/// finished battles can do that, and each search refuses one.
pub(super) fn root_strategy(
    tree: &HashMap<InfoKey, InfoNode>,
    roots: &[InfoKey],
) -> Vec<JointActionProb> {
    let mut index_of: HashMap<JointActionKey, usize> = HashMap::new();
    let mut actions: Vec<Vec<BattleCommand>> = Vec::new();
    let mut sums: Vec<f64> = Vec::new();

    for key in roots {
        let Some(node) = tree.get(key) else {
            continue;
        };
        let weight = node.learner.average_weight();
        let strategy = node.learner.average_strategy();
        for (commands, probability) in node.commands.iter().zip(strategy) {
            let action_key = JointActionKey::new(commands);
            let index = match index_of.get(&action_key) {
                Some(&index) => index,
                None => {
                    let index = actions.len();
                    index_of.insert(action_key, index);
                    actions.push(commands.clone());
                    sums.push(0.0);
                    index
                }
            };
            sums[index] += probability * weight;
        }
    }

    let joint = JointActions {
        total: actions.len(),
        actions,
    };
    let total: f64 = sums.iter().sum();
    if total > 0.0 {
        for probability in &mut sums {
            *probability /= total;
        }
    }
    // A zero floor, not `EPS`: explicit exploration gives every action a real
    // probability, and a large action set can push that probability below `EPS`.
    strategy_of(&joint, &sums, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solver::mcts::SelectionPolicy;

    /// A node must give one index to one joint action, whatever world offered
    /// it. Two worlds that share an action must therefore share its learner
    /// entry.
    #[test]
    fn a_registry_reuses_the_index_of_a_known_action() {
        let mut node = InfoNode::new();
        let pass = vec![vec![BattleCommand::Pass]];
        let both = vec![
            vec![BattleCommand::Pass],
            vec![BattleCommand::Struggle { target: None }],
        ];

        assert_eq!(node.register(&pass), vec![0]);
        assert_eq!(node.register(&both), vec![0, 1]);
        assert_eq!(node.register(&pass), vec![0]);
        assert_eq!(node.commands.len(), 2);
        assert_eq!(node.learner.average_strategy().len(), 2);
    }

    /// A world that offers one of two registered actions must play that one.
    #[test]
    fn a_subset_strategy_covers_only_the_world_actions() {
        let mut node = InfoNode::new();
        node.register(&[
            vec![BattleCommand::Pass],
            vec![BattleCommand::Struggle { target: None }],
        ]);
        let allowed = node.register(&[vec![BattleCommand::Struggle { target: None }]]);

        assert_eq!(allowed, vec![1]);
        let strategy = node
            .learner
            .strategy_subset(SelectionPolicy::RegretMatching, 0.1, &allowed);
        assert_eq!(strategy.len(), 1);
        assert!((strategy[0] - 1.0).abs() < 1e-12, "{strategy:?}");
    }

    /// The root report must use only samples that entered the average.
    #[test]
    fn root_strategy_uses_the_accumulated_average_weight() {
        let first = (ObservationKey::ROOT, 1, 0);
        let second = (ObservationKey::ROOT, 2, 0);
        let actions = [
            vec![BattleCommand::Pass],
            vec![BattleCommand::Struggle { target: None }],
        ];

        let mut first_node = InfoNode::new();
        let first_allowed = first_node.register(&actions);
        first_node
            .learner
            .accumulate_subset_scaled(&first_allowed, &[1.0, 0.0], 1.0);

        let mut second_node = InfoNode::new();
        let second_allowed = second_node.register(&actions);
        second_node
            .learner
            .accumulate_subset_scaled(&second_allowed, &[0.0, 1.0], 9.0);

        let tree = HashMap::from([(first, first_node), (second, second_node)]);
        let strategy = root_strategy(&tree, &[first, second]);

        assert_eq!(strategy.len(), 2);
        assert!((strategy[0].probability - 0.9).abs() < 1e-12, "{strategy:?}");
        assert!((strategy[1].probability - 0.1).abs() < 1e-12, "{strategy:?}");
    }
}
