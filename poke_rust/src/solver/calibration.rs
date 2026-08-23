//! Compares a predicted win probability against a realized game result.
//!
//! `eval::fitted` learns its weights from labels that `solve` produced, and
//! `solve` scores its own horizon with those same weights. Held-out error
//! therefore measures agreement with that loop, not agreement with a game.
//!
//! This module measures the second quantity. A caller plays whole games, scores
//! each position on the way, and records who won. The curve then reports what
//! the evaluator predicted and what happened.
//!
//! The module holds no input and no output, so a test can cover every number
//! that a report prints.
//!
//! # The independent unit is the game
//!
//! Every position of one game carries that one game's result. A bucket that
//! holds 400 positions from 3 games holds 3 independent observations.
//! [`Bucket::games`] reports that count next to the position count, and a
//! report must print both.

use std::collections::HashSet;

/// One scored position and the result of the game that held it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// The predicted P1 win probability, from 0 through 1.
    /// Read it through [`Sample::prediction`], which holds the rule for a value
    /// outside that contract.
    pub predicted: f64,
    /// Whether P1 won the game.
    pub p1_won: bool,
    /// The game that this position came from.
    /// Two samples with the same index are not independent.
    pub game: usize,
}

impl Sample {
    /// Builds one sample.
    pub fn new(predicted: f64, p1_won: bool, game: usize) -> Sample {
        Sample {
            predicted,
            p1_won,
            game,
        }
    }

    /// The prediction that every statistic reads.
    ///
    /// A non-finite value becomes 0.5. The field holds a probability from 0
    /// through 1, so a non-finite value is out of contract. One such value would
    /// otherwise turn a whole report into `NaN` and hide every other position.
    /// One rule here keeps the four printed statistics and the bucket means in
    /// agreement.
    pub fn prediction(&self) -> f64 {
        if self.predicted.is_finite() {
            self.predicted
        } else {
            0.5
        }
    }

    /// The realized value, as 1.0 for a P1 win and 0.0 for a P2 win.
    pub fn realized(&self) -> f64 {
        if self.p1_won { 1.0 } else { 0.0 }
    }
}

/// The number of fixed buckets that a curve reports.
/// Each bucket is 0.1 wide.
pub const BUCKET_COUNT: usize = 10;

/// The smallest probability that [`log_loss`] takes a logarithm of.
/// A prediction of exactly 0 or 1 would otherwise return infinity, and one such
/// prediction would hide every other position.
const LOG_LOSS_FLOOR: f64 = 1e-15;

/// One row of the curve.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    /// The lower edge, which the bucket includes.
    pub low: f64,
    /// The upper edge, which the bucket excludes.
    /// The last bucket includes it, so a prediction of 1.0 has a home.
    pub high: f64,
    /// Positions in this bucket.
    pub positions: usize,
    /// Distinct games that supplied those positions.
    pub games: usize,
    /// The mean prediction of the positions. `None` when the bucket is empty.
    pub mean_predicted: Option<f64>,
    /// The share of the positions whose game P1 won. `None` when the bucket is
    /// empty.
    pub realized: Option<f64>,
}

impl Bucket {
    /// How far the realized rate sits from the mean prediction.
    /// `None` when the bucket is empty.
    pub fn gap(&self) -> Option<f64> {
        match (self.mean_predicted, self.realized) {
            (Some(predicted), Some(realized)) => Some((realized - predicted).abs()),
            _ => None,
        }
    }
}

/// The bucket that one prediction belongs to.
///
/// The value is clamped first, so a prediction outside 0 through 1 still lands
/// in a bucket instead of a panic. A value on an inner edge belongs to the
/// bucket above it, and 1.0 belongs to the last bucket.
///
/// A non-finite value counts as 0.5, which is the rule of
/// [`Sample::prediction`].
pub fn bucket_index(predicted: f64) -> usize {
    if !predicted.is_finite() {
        return BUCKET_COUNT / 2;
    }
    let clamped = predicted.clamp(0.0, 1.0);
    let raw = (clamped * BUCKET_COUNT as f64) as usize;
    raw.min(BUCKET_COUNT - 1)
}

/// The mean absolute error of the predictions.
/// An empty slice returns 0.
pub fn mean_absolute_error(samples: &[Sample]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| (sample.prediction() - sample.realized()).abs())
        .sum();
    total / samples.len() as f64
}

/// The Brier score, which is the mean squared error of the predictions.
/// A constant 0.5 predictor scores 0.25. An empty slice returns 0.
pub fn brier(samples: &[Sample]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| {
            let error = sample.prediction() - sample.realized();
            error * error
        })
        .sum();
    total / samples.len() as f64
}

/// The mean negative log likelihood of the results.
/// A constant 0.5 predictor scores the natural logarithm of 2.
/// An empty slice returns 0.
pub fn log_loss(samples: &[Sample]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total: f64 = samples
        .iter()
        .map(|sample| {
            let predicted = sample
                .prediction()
                .clamp(LOG_LOSS_FLOOR, 1.0 - LOG_LOSS_FLOOR);
            let hit = if sample.p1_won {
                predicted
            } else {
                1.0 - predicted
            };
            -hit.ln()
        })
        .sum();
    total / samples.len() as f64
}

/// The expected calibration error of a bucket list.
///
/// Each nonempty bucket contributes the distance between its mean prediction
/// and its realized rate, weighted by its share of the positions. An empty
/// bucket contributes nothing, and it does not change the weights.
pub fn expected_calibration_error(buckets: &[Bucket]) -> f64 {
    let positions: usize = buckets.iter().map(|bucket| bucket.positions).sum();
    if positions == 0 {
        return 0.0;
    }
    buckets
        .iter()
        .filter_map(|bucket| {
            let gap = bucket.gap()?;
            Some(gap * bucket.positions as f64 / positions as f64)
        })
        .sum()
}

/// The predicted-against-realized curve of one evaluator.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationCurve {
    /// The ten fixed buckets, in order.
    pub buckets: Vec<Bucket>,
    /// Positions that entered the curve.
    pub positions: usize,
    /// Distinct games that supplied those positions.
    pub games: usize,
    /// The mean prediction over every position. `None` when there is none.
    pub mean_predicted: Option<f64>,
    /// The share of positions whose game P1 won. `None` when there is none.
    pub realized: Option<f64>,
    /// The mean absolute error of the predictions.
    pub mean_absolute_error: f64,
    /// The Brier score of the predictions.
    pub brier: f64,
    /// The mean negative log likelihood of the results.
    pub log_loss: f64,
    /// The position-weighted mean bucket gap.
    pub expected_calibration_error: f64,
}

impl CalibrationCurve {
    /// Builds the curve of one sample set.
    ///
    /// An empty set returns ten empty buckets and zero for each statistic.
    pub fn from_samples(samples: &[Sample]) -> CalibrationCurve {
        let mut buckets: Vec<Bucket> = (0..BUCKET_COUNT)
            .map(|index| Bucket {
                low: index as f64 / BUCKET_COUNT as f64,
                high: (index + 1) as f64 / BUCKET_COUNT as f64,
                positions: 0,
                games: 0,
                mean_predicted: None,
                realized: None,
            })
            .collect();

        let mut predicted_total = [0.0f64; BUCKET_COUNT];
        let mut win_total = [0.0f64; BUCKET_COUNT];
        let mut bucket_games: Vec<HashSet<usize>> = vec![HashSet::new(); BUCKET_COUNT];
        let mut all_games: HashSet<usize> = HashSet::new();

        for sample in samples {
            let index = bucket_index(sample.prediction());
            buckets[index].positions += 1;
            predicted_total[index] += sample.prediction();
            win_total[index] += sample.realized();
            bucket_games[index].insert(sample.game);
            all_games.insert(sample.game);
        }

        for index in 0..BUCKET_COUNT {
            let count = buckets[index].positions;
            if count == 0 {
                continue;
            }
            buckets[index].games = bucket_games[index].len();
            buckets[index].mean_predicted = Some(predicted_total[index] / count as f64);
            buckets[index].realized = Some(win_total[index] / count as f64);
        }

        let positions = samples.len();
        let (mean_predicted, realized) = if positions == 0 {
            (None, None)
        } else {
            let predicted: f64 = predicted_total.iter().sum();
            let wins: f64 = win_total.iter().sum();
            (
                Some(predicted / positions as f64),
                Some(wins / positions as f64),
            )
        };

        CalibrationCurve {
            expected_calibration_error: expected_calibration_error(&buckets),
            buckets,
            positions,
            games: all_games.len(),
            mean_predicted,
            realized,
            mean_absolute_error: mean_absolute_error(samples),
            brier: brier(samples),
            log_loss: log_loss(samples),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(pairs: &[(f64, bool)]) -> Vec<Sample> {
        pairs
            .iter()
            .enumerate()
            .map(|(index, (predicted, won))| Sample::new(*predicted, *won, index))
            .collect()
    }

    #[test]
    fn a_perfect_predictor_has_no_calibration_error() {
        let set = samples(&[(1.0, true), (0.0, false), (1.0, true), (0.0, false)]);
        let curve = CalibrationCurve::from_samples(&set);
        assert_eq!(curve.expected_calibration_error, 0.0);
        assert_eq!(curve.mean_absolute_error, 0.0);
        assert_eq!(curve.brier, 0.0);
        assert!(curve.log_loss < 1e-12);
    }

    #[test]
    fn a_constant_half_predictor_scores_one_quarter() {
        let set = samples(&[(0.5, true), (0.5, false), (0.5, true), (0.5, false)]);
        let curve = CalibrationCurve::from_samples(&set);
        assert!((curve.brier - 0.25).abs() < 1e-12);
        assert!((curve.mean_absolute_error - 0.5).abs() < 1e-12);
        assert!((curve.log_loss - 2.0f64.ln()).abs() < 1e-12);
        // A balanced set makes the constant prediction correct on average.
        assert!(curve.expected_calibration_error < 1e-12);
    }

    #[test]
    fn a_sample_on_a_bucket_edge_lands_in_one_bucket() {
        assert_eq!(bucket_index(0.0), 0);
        assert_eq!(bucket_index(0.0999), 0);
        assert_eq!(bucket_index(0.1), 1);
        assert_eq!(bucket_index(0.9), 9);
        assert_eq!(bucket_index(1.0), 9);
        // An out-of-range value and a non-finite value still land somewhere.
        assert_eq!(bucket_index(1.5), 9);
        assert_eq!(bucket_index(-0.5), 0);
        // A non-finite value counts as 0.5, the rule of `Sample::prediction`.
        assert_eq!(bucket_index(f64::NAN), 5);
        assert_eq!(bucket_index(f64::INFINITY), 5);

        let curve = CalibrationCurve::from_samples(&samples(&[(0.1, true)]));
        assert_eq!(curve.buckets[0].positions, 0);
        assert_eq!(curve.buckets[1].positions, 1);
        assert_eq!(curve.positions, 1);
    }

    #[test]
    fn an_empty_bucket_reports_no_rate_and_does_not_enter_the_error() {
        let set = samples(&[(0.05, false), (0.95, false)]);
        let curve = CalibrationCurve::from_samples(&set);
        for bucket in curve.buckets.iter().take(9).skip(1) {
            assert_eq!(bucket.positions, 0);
            assert_eq!(bucket.games, 0);
            assert_eq!(bucket.mean_predicted, None);
            assert_eq!(bucket.realized, None);
            assert_eq!(bucket.gap(), None);
        }
        // Only the two filled buckets carry weight: 0.5 * 0.05 + 0.5 * 0.95.
        assert!((curve.expected_calibration_error - 0.5).abs() < 1e-12);
    }

    #[test]
    fn a_bucket_reports_its_game_count_next_to_its_position_count() {
        let set = vec![
            Sample::new(0.55, true, 7),
            Sample::new(0.56, true, 7),
            Sample::new(0.57, true, 7),
            Sample::new(0.58, false, 9),
        ];
        let curve = CalibrationCurve::from_samples(&set);
        assert_eq!(curve.buckets[5].positions, 4);
        assert_eq!(curve.buckets[5].games, 2);
        assert_eq!(curve.positions, 4);
        assert_eq!(curve.games, 2);
        assert!((curve.realized.unwrap() - 0.75).abs() < 1e-12);
    }

    #[test]
    fn an_empty_sample_set_reports_zero_everywhere() {
        let curve = CalibrationCurve::from_samples(&[]);
        assert_eq!(curve.buckets.len(), BUCKET_COUNT);
        assert_eq!(curve.positions, 0);
        assert_eq!(curve.games, 0);
        assert_eq!(curve.mean_predicted, None);
        assert_eq!(curve.realized, None);
        assert_eq!(curve.brier, 0.0);
        assert_eq!(curve.log_loss, 0.0);
        assert_eq!(curve.mean_absolute_error, 0.0);
        assert_eq!(curve.expected_calibration_error, 0.0);
    }

    #[test]
    fn a_non_finite_prediction_reads_as_one_half_in_every_statistic() {
        // Nothing in the crate produces this today: all three evaluators end in
        // a logistic. `Sample::new` still accepts any `f64`, and one `NaN` used
        // to leave `log_loss` finite while it turned the mean absolute error,
        // the Brier score, the bucket mean, and the curve mean into `NaN`.
        let set = vec![Sample::new(f64::NAN, true, 0), Sample::new(0.5, false, 1)];
        let curve = CalibrationCurve::from_samples(&set);
        assert_eq!(curve.buckets[5].positions, 2);
        assert_eq!(curve.buckets[5].mean_predicted, Some(0.5));
        assert_eq!(curve.mean_predicted, Some(0.5));
        assert!((curve.mean_absolute_error - 0.5).abs() < 1e-12);
        assert!((curve.brier - 0.25).abs() < 1e-12);
        assert!((curve.log_loss - 2.0f64.ln()).abs() < 1e-12);
        assert!(curve.expected_calibration_error < 1e-12);
    }

    #[test]
    fn a_certain_wrong_prediction_stays_finite() {
        let set = samples(&[(1.0, false), (0.0, true)]);
        let curve = CalibrationCurve::from_samples(&set);
        assert!(curve.log_loss.is_finite());
        assert!(curve.log_loss > 30.0);
        assert!((curve.brier - 1.0).abs() < 1e-12);
        assert!((curve.expected_calibration_error - 1.0).abs() < 1e-12);
    }
}
