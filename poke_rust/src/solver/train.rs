//! Fits the linear weight vectors of [`eval`](super::eval).
//!
//! Two models share this module.
//!
//! The value model maps a position feature vector onto P1's win probability.
//! It minimizes the logistic loss against a labeled value.
//!
//! The policy model maps an action feature vector onto a selection probability.
//! It minimizes the cross entropy against a labeled mixture.
//!
//! Both fits use batch gradient descent, a fixed learning rate, and L2
//! regularization.
//! Neither fit needs a machine-learning dependency.
//!
//! `bin/train_eval` builds the labeled sets and writes the results.
//! This module holds the loss, the gradient, the descent loop, and the
//! held-out split, so the unit tests can cover the fit without a corpus.
//!
//! `cargo test` never runs the corpus binary. A corpus costs minutes.

use super::eval::{LOGISTIC_SCALE, logistic, softmax};

/// One labeled position.
#[derive(Debug, Clone)]
pub struct ValueSample<const N: usize> {
    /// The antisymmetric feature vector of the position.
    pub features: [f64; N],
    /// P1's labeled win probability, from zero through one.
    pub label: f64,
}

/// One labeled decision.
#[derive(Debug, Clone)]
pub struct PolicySample<const N: usize> {
    /// One feature vector for each legal action.
    pub actions: Vec<[f64; N]>,
    /// The labeled mixture over those actions. It sums to one.
    pub target: Vec<f64>,
}

/// The settings of one fit.
#[derive(Debug, Clone, Copy)]
pub struct TrainConfig {
    /// Full-batch gradient steps.
    pub steps: usize,
    /// The step size of each descent step.
    pub learning_rate: f64,
    /// The L2 penalty on the weight vector.
    pub l2: f64,
}

impl Default for TrainConfig {
    fn default() -> Self {
        TrainConfig {
            steps: 400,
            learning_rate: 0.5,
            l2: 1e-4,
        }
    }
}

/// Splits a labeled set into a training part and a held-out part.
///
/// The split is deterministic and spreads the held-out items through the set.
/// A deterministic rule makes one run reproducible from its corpus alone.
///
/// A `holdout` of zero or below returns everything as training data.
pub fn split<T: Clone>(items: &[T], holdout: f64) -> (Vec<T>, Vec<T>) {
    if holdout <= 0.0 || items.is_empty() {
        return (items.to_vec(), Vec::new());
    }
    let test_count = ((items.len() as f64 * holdout.min(1.0)).round() as usize)
        .min(items.len().saturating_sub(1));
    if test_count == 0 {
        return (items.to_vec(), Vec::new());
    }
    let mut train = Vec::new();
    let mut test = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let tests_before = index * test_count / items.len();
        let tests_after = (index + 1) * test_count / items.len();
        if tests_after > tests_before {
            test.push(item.clone());
        } else {
            train.push(item.clone());
        }
    }
    (train, test)
}

// ── The value model ─────────────────────────────────────────────────────────

/// P1's predicted win probability for one feature vector.
pub fn value_prediction<const N: usize>(features: &[f64; N], weights: &[f64; N]) -> f64 {
    let advantage: f64 = features
        .iter()
        .zip(weights.iter())
        .map(|(value, weight)| value * weight)
        .sum();
    logistic(advantage)
}

/// The mean logistic loss of one weight vector, without the penalty.
///
/// An empty set returns zero.
pub fn value_loss<const N: usize>(samples: &[ValueSample<N>], weights: &[f64; N]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| {
            let prediction = value_prediction(&sample.features, weights).clamp(1e-12, 1.0 - 1e-12);
            -(sample.label * prediction.ln() + (1.0 - sample.label) * (1.0 - prediction).ln())
        })
        .sum();
    total / samples.len() as f64
}

/// The mean absolute error of one weight vector.
/// The training report uses it, because a probability error reads directly.
pub fn value_mean_absolute_error<const N: usize>(
    samples: &[ValueSample<N>],
    weights: &[f64; N],
) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| (value_prediction(&sample.features, weights) - sample.label).abs())
        .sum();
    total / samples.len() as f64
}

/// The gradient of [`value_loss`] plus the L2 penalty.
pub fn value_gradient<const N: usize>(
    samples: &[ValueSample<N>],
    weights: &[f64; N],
    l2: f64,
) -> [f64; N] {
    let mut gradient = [0.0; N];
    if samples.is_empty() {
        return gradient;
    }
    for sample in samples {
        let prediction = value_prediction(&sample.features, weights);
        let residual = (prediction - sample.label) * LOGISTIC_SCALE;
        for (value, feature) in gradient.iter_mut().zip(sample.features.iter()) {
            *value += residual * feature;
        }
    }
    normalize(&mut gradient, samples.len() as f64, weights, l2);
    gradient
}

/// Divides a summed gradient by the sample count and adds the L2 term.
fn normalize<const N: usize>(gradient: &mut [f64; N], count: f64, weights: &[f64; N], l2: f64) {
    for (value, weight) in gradient.iter_mut().zip(weights.iter()) {
        *value = *value / count + l2 * weight;
    }
}

/// Fits the value weights, starting from `start`.
///
/// A step that produces a nonfinite weight is discarded, and the fit returns
/// the last finite vector. A discarded step keeps a bad learning rate from
/// writing a broken weight file.
pub fn fit_value<const N: usize>(
    samples: &[ValueSample<N>],
    start: &[f64; N],
    config: &TrainConfig,
) -> [f64; N] {
    let mut weights = *start;
    for _ in 0..config.steps {
        let gradient = value_gradient(samples, &weights, config.l2);
        let mut next = weights;
        for (value, step) in next.iter_mut().zip(gradient.iter()) {
            *value -= config.learning_rate * step;
        }
        if next.iter().any(|value| !value.is_finite()) {
            return weights;
        }
        weights = next;
    }
    weights
}

// ── The policy model ────────────────────────────────────────────────────────

/// The softmax distribution of one decision under one weight vector.
pub fn policy_prediction<const N: usize>(
    actions: &[[f64; N]],
    weights: &[f64; N],
) -> Vec<f64> {
    let scores: Vec<f64> = actions
        .iter()
        .map(|features| {
            features
                .iter()
                .zip(weights.iter())
                .map(|(value, weight)| value * weight)
                .sum()
        })
        .collect();
    softmax(&scores)
}

/// The mean cross entropy of one weight vector, without the penalty.
pub fn policy_loss<const N: usize>(samples: &[PolicySample<N>], weights: &[f64; N]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| {
            let prediction = policy_prediction(&sample.actions, weights);
            -sample
                .target
                .iter()
                .zip(prediction.iter())
                .map(|(target, predicted)| target * predicted.max(1e-12).ln())
                .sum::<f64>()
        })
        .sum();
    total / samples.len() as f64
}

/// The gradient of [`policy_loss`] plus the L2 penalty.
pub fn policy_gradient<const N: usize>(
    samples: &[PolicySample<N>],
    weights: &[f64; N],
    l2: f64,
) -> [f64; N] {
    let mut gradient = [0.0; N];
    if samples.is_empty() {
        return gradient;
    }
    for sample in samples {
        let prediction = policy_prediction(&sample.actions, weights);
        for (action_index, features) in sample.actions.iter().enumerate() {
            let target = sample.target.get(action_index).copied().unwrap_or(0.0);
            let residual = prediction[action_index] - target;
            for (value, feature) in gradient.iter_mut().zip(features.iter()) {
                *value += residual * feature;
            }
        }
    }
    normalize(&mut gradient, samples.len() as f64, weights, l2);
    gradient
}

/// Fits the policy weights, starting from `start`.
/// A nonfinite step ends the fit, as it does in [`fit_value`].
pub fn fit_policy<const N: usize>(
    samples: &[PolicySample<N>],
    start: &[f64; N],
    config: &TrainConfig,
) -> [f64; N] {
    let mut weights = *start;
    for _ in 0..config.steps {
        let gradient = policy_gradient(samples, &weights, config.l2);
        let mut next = weights;
        for (value, step) in next.iter_mut().zip(gradient.iter()) {
            *value -= config.learning_rate * step;
        }
        if next.iter().any(|value| !value.is_finite()) {
            return weights;
        }
        weights = next;
    }
    weights
}

/// How often the top action of a policy is the top action of a label.
///
/// The training report uses it, because a ranking measure reads more directly
/// than a cross entropy.
pub fn policy_top_agreement<const N: usize>(
    samples: &[PolicySample<N>],
    weights: &[f64; N],
) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let agreed = samples
        .iter()
        .filter(|sample| {
            let prediction = policy_prediction(&sample.actions, weights);
            match (argmax(&prediction), argmax(&sample.target)) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            }
        })
        .count();
    agreed as f64 / samples.len() as f64
}

/// The index of the largest value.
/// Returns `None` for an empty slice.
pub fn argmax(values: &[f64]) -> Option<usize> {
    values
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set that one feature separates cleanly.
    fn separable() -> Vec<ValueSample<2>> {
        (0..20)
            .map(|index| {
                let sign = if index % 2 == 0 { 1.0 } else { -1.0 };
                ValueSample {
                    features: [sign, 0.0],
                    label: if sign > 0.0 { 1.0 } else { 0.0 },
                }
            })
            .collect()
    }

    #[test]
    fn gradient_descent_lowers_the_loss_on_a_separable_set() {
        let samples = separable();
        let start = [0.0, 0.0];
        let before = value_loss(&samples, &start);
        let config = TrainConfig {
            steps: 500,
            learning_rate: 1.0,
            l2: 0.0,
        };
        let fitted = fit_value(&samples, &start, &config);
        let after = value_loss(&samples, &fitted);
        assert!(after < before, "loss rose: {before} -> {after}");
        assert!(fitted[0] > 0.0, "the separating weight kept its sign");
    }

    #[test]
    fn the_value_gradient_matches_a_finite_difference() {
        let samples = separable();
        let weights = [0.3, -0.2];
        let analytic = value_gradient(&samples, &weights, 0.0);
        let step = 1e-6;
        for index in 0..2 {
            let mut up = weights;
            let mut down = weights;
            up[index] += step;
            down[index] -= step;
            let numeric = (value_loss(&samples, &up) - value_loss(&samples, &down)) / (2.0 * step);
            assert!(
                (numeric - analytic[index]).abs() < 1e-6,
                "feature {index}: {numeric} vs {}",
                analytic[index]
            );
        }
    }

    #[test]
    fn the_policy_gradient_matches_a_finite_difference() {
        let samples = vec![PolicySample::<2> {
            actions: vec![[1.0, 0.0], [0.0, 1.0], [0.5, 0.5]],
            target: vec![0.6, 0.3, 0.1],
        }];
        let weights = [0.4, -0.1];
        let analytic = policy_gradient(&samples, &weights, 0.0);
        let step = 1e-6;
        for index in 0..2 {
            let mut up = weights;
            let mut down = weights;
            up[index] += step;
            down[index] -= step;
            let numeric = (policy_loss(&samples, &up) - policy_loss(&samples, &down)) / (2.0 * step);
            assert!(
                (numeric - analytic[index]).abs() < 1e-6,
                "feature {index}: {numeric} vs {}",
                analytic[index]
            );
        }
    }

    #[test]
    fn the_policy_fit_moves_toward_the_labeled_mixture() {
        let samples = vec![PolicySample::<2> {
            actions: vec![[1.0, 0.0], [0.0, 1.0]],
            target: vec![0.9, 0.1],
        }];
        let start = [0.0, 0.0];
        let before = policy_loss(&samples, &start);
        let fitted = fit_policy(&samples, &start, &TrainConfig::default());
        let after = policy_loss(&samples, &fitted);
        assert!(after < before, "loss rose: {before} -> {after}");
        let prediction = policy_prediction(&samples[0].actions, &fitted);
        assert!(prediction[0] > prediction[1]);
    }

    #[test]
    fn the_split_holds_out_a_fifth_and_keeps_every_item() {
        let items: Vec<usize> = (0..20).collect();
        let (train, test) = split(&items, 0.2);
        assert_eq!(test.len(), 4);
        assert_eq!(train.len(), 16);
        assert_eq!(train.len() + test.len(), items.len());
    }

    #[test]
    fn the_split_uses_the_requested_fraction_above_one_half() {
        let items: Vec<usize> = (0..10).collect();
        let (train, test) = split(&items, 0.8);
        assert_eq!(test.len(), 8);
        assert_eq!(train.len(), 2);
        assert_eq!(train.len() + test.len(), items.len());
    }

    #[test]
    fn the_split_keeps_training_data_for_a_valid_holdout() {
        let items = vec![1, 2];
        let (train, test) = split(&items, 0.9);
        assert_eq!(train.len(), 1);
        assert_eq!(test.len(), 1);
    }

    #[test]
    fn a_zero_holdout_keeps_everything_for_training() {
        let items: Vec<usize> = (0..7).collect();
        let (train, test) = split(&items, 0.0);
        assert_eq!(train.len(), 7);
        assert!(test.is_empty());
    }
}
