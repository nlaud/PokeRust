//! Fits the weights of [`eval`](super::eval).
//!
//! Three models share this module.
//!
//! The value model maps a position feature vector onto P1's win probability.
//! It minimizes the logistic loss against a labeled value.
//!
//! The policy model maps an action feature vector onto a selection probability.
//! It minimizes the cross entropy against a labeled mixture.
//!
//! The network model reads the same feature vector through one hidden layer.
//! [`fit_mlp`] answers the question that [`learning_curve`] asks: a flat curve
//! shows that more positions no longer help the linear model.
//!
//! Every fit uses batch gradient descent, a fixed learning rate, and L2
//! regularization.
//! No fit needs a machine-learning dependency.
//!
//! `bin/train_eval` builds the labeled sets and writes the results.
//! This module holds the loss, the gradient, the descent loop, and the
//! held-out split, so the unit tests can cover the fit without a corpus.
//!
//! `cargo test` never runs the corpus binary. A corpus costs minutes.

use super::eval::{LOGISTIC_SCALE, Mlp, logistic, softmax};

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

// ── The network model ───────────────────────────────────────────────────────

/// The mean logistic loss of one network, without the penalty.
pub fn mlp_loss<const N: usize, const H: usize>(
    samples: &[ValueSample<N>],
    network: &Mlp<N, H>,
) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| {
            let prediction = network.predict(&sample.features).clamp(1e-12, 1.0 - 1e-12);
            -(sample.label * prediction.ln() + (1.0 - sample.label) * (1.0 - prediction).ln())
        })
        .sum();
    total / samples.len() as f64
}

/// The mean absolute error of one network.
pub fn mlp_mean_absolute_error<const N: usize, const H: usize>(
    samples: &[ValueSample<N>],
    network: &Mlp<N, H>,
) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| (network.predict(&sample.features) - sample.label).abs())
        .sum();
    total / samples.len() as f64
}

/// The gradient of [`mlp_loss`] plus the L2 penalty.
///
/// The output layer takes the same residual as the linear model.
/// Each hidden row takes that residual through its own output weight and the
/// derivative of `tanh`, which is `1 - tanh(z)^2`.
pub fn mlp_gradient<const N: usize, const H: usize>(
    samples: &[ValueSample<N>],
    network: &Mlp<N, H>,
    l2: f64,
) -> Mlp<N, H> {
    let mut gradient = Mlp {
        hidden: [[0.0; N]; H],
        output: [0.0; H],
    };
    if samples.is_empty() {
        return gradient;
    }
    for sample in samples {
        let activations = network.activations(&sample.features);
        let prediction = logistic(
            activations
                .iter()
                .zip(network.output.iter())
                .map(|(activation, weight)| activation * weight)
                .sum(),
        );
        let residual = (prediction - sample.label) * LOGISTIC_SCALE;
        for (unit, activation) in activations.iter().enumerate() {
            gradient.output[unit] += residual * activation;
            let inner = residual * network.output[unit] * (1.0 - activation.powi(2));
            for (slot, feature) in gradient.hidden[unit]
                .iter_mut()
                .zip(sample.features.iter())
            {
                *slot += inner * feature;
            }
        }
    }

    let count = samples.len() as f64;
    for unit in 0..H {
        gradient.output[unit] = gradient.output[unit] / count + l2 * network.output[unit];
        for feature in 0..N {
            gradient.hidden[unit][feature] =
                gradient.hidden[unit][feature] / count + l2 * network.hidden[unit][feature];
        }
    }
    gradient
}

/// Fits the network, starting from `start`.
///
/// A step that produces a nonfinite weight is discarded, and the fit returns the
/// last finite network, exactly as [`fit_value`] does.
pub fn fit_mlp<const N: usize, const H: usize>(
    samples: &[ValueSample<N>],
    start: &Mlp<N, H>,
    config: &TrainConfig,
) -> Mlp<N, H> {
    let mut network = *start;
    for _ in 0..config.steps {
        let gradient = mlp_gradient(samples, &network, config.l2);
        let mut next = network;
        for unit in 0..H {
            next.output[unit] -= config.learning_rate * gradient.output[unit];
            for feature in 0..N {
                next.hidden[unit][feature] -=
                    config.learning_rate * gradient.hidden[unit][feature];
            }
        }
        if !next.is_finite() {
            return network;
        }
        network = next;
    }
    network
}

// ── Corpus reports ──────────────────────────────────────────────────────────

/// One point of a learning curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvePoint {
    /// The fraction of the training set that this point used.
    pub fraction: f64,
    /// How many samples that fraction held.
    pub samples: usize,
    /// The held-out mean absolute error of the fit.
    pub holdout_error: f64,
}

/// A deterministic subset that spreads its picks through `items`.
///
/// A prefix would take one part of the corpus, and the corpus is ordered by
/// generated matchup. A spread subset keeps every matchup represented.
pub fn subset<T: Clone>(items: &[T], fraction: f64) -> Vec<T> {
    if !fraction.is_finite() || fraction <= 0.0 || items.is_empty() {
        return Vec::new();
    }
    if fraction >= 1.0 {
        return items.to_vec();
    }
    let wanted = ((items.len() as f64 * fraction).round() as usize).clamp(1, items.len());
    (0..items.len())
        .filter(|index| {
            (index + 1) * wanted / items.len() > index * wanted / items.len()
        })
        .map(|index| items[index].clone())
        .collect()
}

/// Fits the linear model on each fraction of the training set.
///
/// A curve that stops falling shows that more positions no longer help the
/// linear model. That is the evidence the model-class question needs.
pub fn learning_curve<const N: usize>(
    train_set: &[ValueSample<N>],
    test_set: &[ValueSample<N>],
    start: &[f64; N],
    config: &TrainConfig,
    fractions: &[f64],
) -> Vec<CurvePoint> {
    fractions
        .iter()
        .map(|fraction| {
            let part = subset(train_set, *fraction);
            let weights = fit_value(&part, start, config);
            CurvePoint {
                fraction: *fraction,
                samples: part.len(),
                holdout_error: value_mean_absolute_error(test_set, &weights),
            }
        })
        .collect()
}

/// The variance of each feature across a labeled set.
///
/// A feature with zero variance does not explain differences between samples.
pub fn feature_variance<const N: usize>(samples: &[ValueSample<N>]) -> [f64; N] {
    let mut out = [0.0; N];
    if samples.is_empty() {
        return out;
    }
    let count = samples.len() as f64;
    for (index, slot) in out.iter_mut().enumerate() {
        let mean: f64 = samples
            .iter()
            .map(|sample| sample.features[index])
            .sum::<f64>()
            / count;
        *slot = samples
            .iter()
            .map(|sample| (sample.features[index] - mean).powi(2))
            .sum::<f64>()
            / count;
    }
    out
}

/// The Pearson correlation of two features across a labeled set.
///
/// Returns zero when either feature is constant, because a constant feature has
/// no correlation to report.
pub fn feature_correlation<const N: usize>(
    samples: &[ValueSample<N>],
    left: usize,
    right: usize,
) -> f64 {
    if samples.is_empty() || left >= N || right >= N {
        return 0.0;
    }
    let count = samples.len() as f64;
    let mean = |index: usize| {
        samples
            .iter()
            .map(|sample| sample.features[index])
            .sum::<f64>()
            / count
    };
    let left_mean = mean(left);
    let right_mean = mean(right);

    let mut covariance = 0.0;
    let mut left_spread = 0.0;
    let mut right_spread = 0.0;
    for sample in samples {
        let left_delta = sample.features[left] - left_mean;
        let right_delta = sample.features[right] - right_mean;
        covariance += left_delta * right_delta;
        left_spread += left_delta * left_delta;
        right_spread += right_delta * right_delta;
    }
    if left_spread <= 0.0 || right_spread <= 0.0 {
        return 0.0;
    }
    covariance / (left_spread * right_spread).sqrt()
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

    // ── The network model ───────────────────────────────────────────────────

    #[test]
    fn the_network_forward_pass_matches_a_hand_calculation() {
        let network = Mlp::<2, 2> {
            hidden: [[1.0, 0.0], [0.0, 2.0]],
            output: [0.5, -1.0],
        };
        let features = [0.5, 0.25];
        // Both units see 0.5, so the advantage is (0.5 - 1.0) * tanh(0.5).
        let advantage = -0.5 * 0.5f64.tanh();
        assert!((network.advantage(&features) - advantage).abs() < 1e-12);
        let expected = 1.0 / (1.0 + (-LOGISTIC_SCALE * advantage).exp());
        assert!((network.predict(&features) - expected).abs() < 1e-12);
    }

    #[test]
    fn the_network_gradient_matches_a_finite_difference() {
        let samples = separable();
        let network = Mlp::<2, 3> {
            hidden: [[0.4, -0.2], [0.1, 0.3], [-0.5, 0.25]],
            output: [0.7, -0.4, 0.2],
        };
        let analytic = mlp_gradient(&samples, &network, 0.0);
        let step = 1e-6;

        for unit in 0..3 {
            let mut up = network;
            let mut down = network;
            up.output[unit] += step;
            down.output[unit] -= step;
            let numeric = (mlp_loss(&samples, &up) - mlp_loss(&samples, &down)) / (2.0 * step);
            assert!(
                (numeric - analytic.output[unit]).abs() < 1e-6,
                "output {unit}: {numeric} vs {}",
                analytic.output[unit]
            );

            for feature in 0..2 {
                let mut up = network;
                let mut down = network;
                up.hidden[unit][feature] += step;
                down.hidden[unit][feature] -= step;
                let numeric = (mlp_loss(&samples, &up) - mlp_loss(&samples, &down)) / (2.0 * step);
                assert!(
                    (numeric - analytic.hidden[unit][feature]).abs() < 1e-6,
                    "hidden {unit},{feature}: {numeric} vs {}",
                    analytic.hidden[unit][feature]
                );
            }
        }
    }

    #[test]
    fn the_network_fit_lowers_the_loss_on_a_separable_set() {
        let samples = separable();
        let start = Mlp::<2, 2>::seed(&[0.0, 0.0]);
        // A seed from zero weights has no output layer, so the fit must build one
        // from the hidden layer that the seed does supply.
        let start = Mlp::<2, 2> {
            hidden: start.hidden,
            output: [0.1, -0.1],
        };
        let before = mlp_loss(&samples, &start);
        let config = TrainConfig {
            steps: 500,
            learning_rate: 1.0,
            l2: 0.0,
        };
        let fitted = fit_mlp(&samples, &start, &config);
        let after = mlp_loss(&samples, &fitted);
        assert!(after < before, "loss rose: {before} -> {after}");
        assert!(fitted.is_finite());
    }

    #[test]
    fn the_seed_network_starts_at_the_linear_model() {
        let weights = [0.6, -0.3];
        let network = Mlp::<2, 2>::seed(&weights);
        // tanh is close to the identity near zero, so a small feature vector
        // reads almost the same as it does through the linear model.
        let features = [0.05, -0.05];
        let linear = value_prediction(&features, &weights);
        assert!((network.predict(&features) - linear).abs() < 1e-3);
    }

    #[test]
    fn a_wide_seed_does_not_multiply_the_linear_weights() {
        let weights = [0.6, -0.3];
        let network = Mlp::<2, 5>::seed(&weights);
        let features = [0.05, -0.05];
        let linear = value_prediction(&features, &weights);
        assert!((network.predict(&features) - linear).abs() < 1e-3);
    }

    #[test]
    #[should_panic(expected = "at least one unit per feature")]
    fn a_seed_rejects_a_hidden_layer_that_cannot_cover_the_features() {
        let _ = Mlp::<2, 1>::seed(&[0.6, -0.3]);
    }

    // ── Corpus reports ──────────────────────────────────────────────────────

    #[test]
    fn the_learning_curve_returns_one_point_for_each_fraction() {
        let samples = separable();
        let (train_set, test_set) = split(&samples, 0.2);
        let fractions = [0.25, 0.5, 0.75, 1.0];
        let curve = learning_curve(
            &train_set,
            &test_set,
            &[0.0, 0.0],
            &TrainConfig::default(),
            &fractions,
        );
        assert_eq!(curve.len(), fractions.len());
        for (point, fraction) in curve.iter().zip(fractions.iter()) {
            assert_eq!(point.fraction, *fraction);
            assert!(point.samples > 0);
            assert!(point.holdout_error.is_finite());
        }
        assert!(curve[3].samples > curve[0].samples);
    }

    #[test]
    fn a_subset_spreads_its_picks_and_keeps_the_requested_count() {
        let items: Vec<usize> = (0..20).collect();
        let half = subset(&items, 0.5);
        assert_eq!(half.len(), 10);
        assert!(half.contains(&0) || half.contains(&1));
        assert!(half.iter().any(|value| *value >= 15), "the tail was dropped");
        assert_eq!(subset(&items, 1.0).len(), 20);
        assert!(subset(&items, 0.0).is_empty());
    }

    #[test]
    fn the_variance_of_a_constant_feature_is_zero() {
        let samples: Vec<ValueSample<2>> = (0..10)
            .map(|index| ValueSample {
                features: [index as f64, 3.0],
                label: 0.5,
            })
            .collect();
        let variance = feature_variance(&samples);
        assert!(variance[0] > 0.0);
        assert!(variance[1].abs() < 1e-12, "a constant feature varied");
    }

    #[test]
    fn the_correlation_matches_a_hand_calculation() {
        // The second feature is twice the first, so the correlation is exactly 1.
        let doubled: Vec<ValueSample<2>> = (0..5)
            .map(|index| ValueSample {
                features: [index as f64, 2.0 * index as f64],
                label: 0.5,
            })
            .collect();
        assert!((feature_correlation(&doubled, 0, 1) - 1.0).abs() < 1e-12);

        // A negated feature gives exactly -1.
        let negated: Vec<ValueSample<2>> = (0..5)
            .map(|index| ValueSample {
                features: [index as f64, -(index as f64)],
                label: 0.5,
            })
            .collect();
        assert!((feature_correlation(&negated, 0, 1) + 1.0).abs() < 1e-12);

        // A constant feature reports zero instead of a division by zero.
        let constant: Vec<ValueSample<2>> = (0..5)
            .map(|index| ValueSample {
                features: [index as f64, 1.0],
                label: 0.5,
            })
            .collect();
        assert_eq!(feature_correlation(&constant, 0, 1), 0.0);
    }
}
