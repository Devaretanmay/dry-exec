//! System-One decision layer: deterministic routing of a state delta onto an escalation state.
//!
//! The layer evaluates the mathematical receipt of an ephemeral state mutation against calibrated
//! environment thresholds. It performs no I/O, invokes no language model, and reads only
//! [`DeltaSummary`], a `Copy` scalar projection of the state delta. The scored region therefore
//! cannot reach the mutation vectors, so the decision is O(1) in the size of the state by
//! construction rather than by convention.

pub mod environment;
pub mod laya_scorer;
pub mod primitives;

pub use environment::{DeltaSummary, Environment};
#[cfg(feature = "laya")]
pub use laya_scorer::LayaScorer;
pub use laya_scorer::NeuralScoringStrategy;
pub use primitives::{Choice, DecisionReceipt, Noul, Score};

/// Calibrated weighting of the mutation-volume term of the risk score.
const BYTE_TERM_WEIGHT: f32 = 0.5;

/// Calibrated weighting of the intercepted-call term of the risk score.
const NETWORK_TERM_WEIGHT: f32 = 0.5;

/// Unsaturated ratio of an observed count against a calibrated ceiling.
///
/// A zero ceiling is treated as an exhausted budget: no mutation scores `0.0`, any mutation
/// saturates, which routes deterministically instead of dividing by zero.
fn ratio(observed: usize, ceiling: usize) -> f32 {
    if ceiling == 0 {
        return if observed == 0 { 0.0 } else { f32::INFINITY };
    }
    observed as f32 / ceiling as f32
}

/// Computes the System-One decision receipt from the state delta summary and environment rules.
///
/// The risk score is the calibrated weighted ratio of the mutation volume and the intercepted
/// call count against their ceilings. Routing is categorical first: a breached mutation ceiling
/// yields [`Choice::Violated`], a refused route yields [`Choice::Blocked`], and only an
/// [`Choice::Allowed`] delta is additionally routed by [`Environment::max_risk_threshold`].
pub fn evaluate(summary: &DeltaSummary, environment: &Environment) -> DecisionReceipt {
    let bytes_ratio = ratio(summary.total_bytes_mutated, environment.max_mutated_bytes);
    let network_ratio = ratio(summary.network_calls, environment.max_network_calls);
    let risk_score =
        Score::new(bytes_ratio * BYTE_TERM_WEIGHT + network_ratio * NETWORK_TERM_WEIGHT);

    let choice = if bytes_ratio > Score::MAX || network_ratio > Score::MAX {
        Choice::Violated
    } else if summary.schema_breaches > 0 {
        Choice::Blocked
    } else {
        Choice::Allowed
    };

    let noul_trigger =
        if choice == Choice::Allowed && risk_score.value() <= environment.max_risk_threshold {
            Noul::AutoCommit
        } else {
            Noul::Escalate
        };

    let reason = match (choice, noul_trigger) {
        (Choice::Violated, _) => format!(
            "Mutation volume {} bytes exceeds calibrated ceiling {} bytes",
            summary.total_bytes_mutated, environment.max_mutated_bytes
        ),
        (Choice::Blocked, _) => format!(
            "Transparent proxy refused {} route(s) outside the registered schema",
            summary.schema_breaches
        ),
        (Choice::Allowed, Noul::Escalate) => format!(
            "Risk score {:.2} exceeds calibrated threshold {:.2}",
            risk_score.value(),
            environment.max_risk_threshold
        ),
        (Choice::Allowed, Noul::AutoCommit) => format!(
            "Risk score {:.2} within calibrated threshold {:.2}; {} bytes across {} page(s), {} intercepted call(s)",
            risk_score.value(),
            environment.max_risk_threshold,
            summary.total_bytes_mutated,
            summary.memory_pages_mutated,
            summary.network_calls
        ),
    };

    DecisionReceipt {
        choice,
        risk_score,
        noul_trigger,
        reason,
    }
}

/// Evaluates an allowed delta with an optional local neural scorer.
///
/// Deterministic refusals never invoke the scorer. If a scorer is present, its calibrated risk
/// contributes 60% of the final score while the deterministic score contributes 40%.
pub fn evaluate_with_neural(
    summary: &DeltaSummary,
    environment: &Environment,
    scorer: Option<&dyn NeuralScoringStrategy>,
    task: &str,
    semantic_summary: &str,
) -> DecisionReceipt {
    let deterministic = evaluate(summary, environment);
    if deterministic.choice != Choice::Allowed {
        return deterministic;
    }

    let Some(scorer) = scorer else {
        return deterministic;
    };

    let neural_risk = Score::new(scorer.score(task, semantic_summary));
    let risk_score = Score::new(deterministic.risk_score.value() * 0.4 + neural_risk.value() * 0.6);
    let noul_trigger = if risk_score.value() <= environment.max_risk_threshold {
        Noul::AutoCommit
    } else {
        Noul::Escalate
    };
    let reason = match noul_trigger {
        Noul::AutoCommit => format!(
            "Risk score {:.2}; deterministic rules passed and local System-One analysis stayed within threshold {:.2}",
            risk_score.value(),
            environment.max_risk_threshold
        ),
        Noul::Escalate => format!(
            "Risk score {:.2}; deterministic rules passed but local System-One analysis exceeded threshold {:.2}",
            risk_score.value(),
            environment.max_risk_threshold
        ),
    };

    DecisionReceipt {
        choice: Choice::Allowed,
        risk_score,
        noul_trigger,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::{PageMutation, StateDelta};
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    struct FixedScorer {
        value: f32,
        calls: Cell<usize>,
    }

    impl NeuralScoringStrategy for FixedScorer {
        fn score(&self, _task: &str, _semantic_summary: &str) -> f32 {
            self.calls.set(self.calls.get() + 1);
            self.value
        }
    }

    /// Page count whose 4KB granularity accounts for roughly 100MB of mutation volume.
    const HUNDRED_MEGABYTE_PAGES: usize = 25_600;

    /// A state delta reporting roughly 100MB of mutated memory across 4KB pages.
    fn hundred_megabyte_delta() -> StateDelta {
        let page = PageMutation {
            page_index: 0,
            page_address: 0,
            deltas: Vec::new(),
        };
        StateDelta {
            memory_mutations: vec![page; HUNDRED_MEGABYTE_PAGES],
            total_bytes_mutated: HUNDRED_MEGABYTE_PAGES * 4096,
            ..Default::default()
        }
    }

    /// Mean `evaluate` latency for a delta, measured over a fixed iteration count.
    fn mean_evaluate_latency(delta: &StateDelta) -> Duration {
        let summary = DeltaSummary::from(delta);
        let environment = Environment::default();
        let iterations = 1_000u32;

        let start = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(evaluate(
                std::hint::black_box(&summary),
                std::hint::black_box(&environment),
            ));
        }
        start.elapsed() / iterations
    }

    #[test]
    fn test_calibrated_defaults_match_environment_schema() {
        let environment = Environment::default();
        assert_eq!(environment.max_mutated_bytes, 1_048_576);
        assert_eq!(environment.max_network_calls, 16);
        assert_eq!(environment.max_risk_threshold, 0.5);
    }

    #[test]
    fn test_score_clamps_and_saturates_non_finite() {
        assert_eq!(Score::new(-4.0).value(), Score::MIN);
        assert_eq!(Score::new(0.42).value(), 0.42);
        assert_eq!(Score::new(4.0).value(), Score::MAX);
        assert_eq!(Score::new(f32::INFINITY).value(), Score::MAX);
        assert_eq!(Score::new(f32::NAN).value(), Score::MAX);
        assert_eq!(Score::default().value(), Score::MIN);
    }

    #[test]
    fn test_evaluate_routes_calibrated_delta_to_auto_commit() {
        let summary = DeltaSummary {
            total_bytes_mutated: 4,
            memory_pages_mutated: 1,
            ..Default::default()
        };

        let receipt = evaluate(&summary, &Environment::default());

        assert_eq!(receipt.choice, Choice::Allowed);
        assert_eq!(receipt.noul_trigger, Noul::AutoCommit);
        assert!(receipt.risk_score.value() < 0.01);
        assert!(receipt.reason.contains("within calibrated threshold"));
    }

    #[test]
    fn test_evaluate_escalates_allowed_delta_above_threshold() {
        // Both calibrated ratios stay within their ceilings, so the categorical choice holds;
        // the aggregate reaches 0.75 and the threshold term alone routes to escalation.
        let summary = DeltaSummary {
            total_bytes_mutated: 1_048_576,
            network_calls: 8,
            ..Default::default()
        };

        let receipt = evaluate(&summary, &Environment::default());

        assert_eq!(receipt.choice, Choice::Allowed);
        assert_eq!(receipt.noul_trigger, Noul::Escalate);
        assert!((receipt.risk_score.value() - 0.75).abs() < f32::EPSILON);
        assert!(receipt.reason.contains("exceeds calibrated threshold"));
    }

    #[test]
    fn test_evaluate_violates_breached_mutation_ceiling() {
        let summary = DeltaSummary {
            total_bytes_mutated: 8,
            network_calls: 1,
            ..Default::default()
        };
        let environment = Environment {
            max_mutated_bytes: 4,
            max_network_calls: 4,
            max_risk_threshold: 0.9,
        };

        let receipt = evaluate(&summary, &environment);

        assert_eq!(receipt.choice, Choice::Violated);
        assert_eq!(receipt.noul_trigger, Noul::Escalate);
        assert!(receipt.reason.contains("exceeds calibrated ceiling"));
    }

    #[test]
    fn test_evaluate_blocks_on_refused_route() {
        let summary = DeltaSummary {
            schema_breaches: 2,
            total_bytes_mutated: 12,
            ..Default::default()
        };

        let receipt = evaluate(&summary, &Environment::default());

        assert_eq!(receipt.choice, Choice::Blocked);
        assert_eq!(receipt.noul_trigger, Noul::Escalate);
        assert!(receipt.reason.contains("refused 2 route(s)"));
    }

    #[test]
    fn test_evaluate_exhausted_ceiling_routes_without_dividing_by_zero() {
        let environment = Environment {
            max_mutated_bytes: 0,
            max_network_calls: 0,
            max_risk_threshold: 0.5,
        };

        let empty = evaluate(&DeltaSummary::default(), &environment);
        assert_eq!(empty.choice, Choice::Allowed);
        assert_eq!(empty.noul_trigger, Noul::AutoCommit);
        assert_eq!(empty.risk_score.value(), Score::MIN);

        let mutated = evaluate(
            &DeltaSummary {
                total_bytes_mutated: 1,
                ..Default::default()
            },
            &environment,
        );
        assert_eq!(mutated.choice, Choice::Violated);
        assert_eq!(mutated.risk_score.value(), Score::MAX);
    }

    #[test]
    fn test_hybrid_path_short_circuits_deterministic_refusal() {
        let scorer = FixedScorer {
            value: 1.0,
            calls: Cell::new(0),
        };
        let receipt = evaluate_with_neural(
            &DeltaSummary {
                schema_breaches: 1,
                ..Default::default()
            },
            &Environment::default(),
            Some(&scorer),
            "delete production data",
            "schema route rejected",
        );

        assert_eq!(receipt.choice, Choice::Blocked);
        assert_eq!(scorer.calls.get(), 0);
    }

    #[test]
    fn test_hybrid_path_blends_deterministic_and_local_scores() {
        let scorer = FixedScorer {
            value: 1.0,
            calls: Cell::new(0),
        };
        let receipt = evaluate_with_neural(
            &DeltaSummary::default(),
            &Environment::default(),
            Some(&scorer),
            "inspect report",
            "read-only summary",
        );

        assert_eq!(receipt.choice, Choice::Allowed);
        assert_eq!(receipt.noul_trigger, Noul::Escalate);
        assert!((receipt.risk_score.value() - 0.6).abs() < f32::EPSILON);
        assert_eq!(scorer.calls.get(), 1);
    }

    #[test]
    fn test_decision_latency_bound_for_hundred_megabyte_delta() {
        let delta = hundred_megabyte_delta();
        assert_eq!(delta.total_bytes_mutated, 104_857_600);

        let projection_start = Instant::now();
        let summary = DeltaSummary::from(&delta);
        let projection = projection_start.elapsed();

        assert_eq!(summary.memory_pages_mutated, HUNDRED_MEGABYTE_PAGES);
        assert!(
            projection.as_micros() < 100,
            "summary projection {} us exceeded the 100 us (0.1ms) bound",
            projection.as_micros()
        );

        let mean = mean_evaluate_latency(&delta);
        assert!(
            mean.as_micros() < 100,
            "evaluate() mean of {} ns exceeded the 100 us (0.1ms) bound",
            mean.as_nanos()
        );
    }

    #[test]
    fn test_decision_latency_is_invariant_across_state_size() {
        let large = mean_evaluate_latency(&hundred_megabyte_delta());
        let trivial = mean_evaluate_latency(&StateDelta::default());

        // Scalar routing cannot scale with state size; the tolerance absorbs timer noise on
        // sub-microsecond measurements rather than permitting linear growth.
        let bound = trivial.mul_f32(4.0) + Duration::from_micros(10);
        assert!(
            large < bound,
            "evaluate() mean of {} ns over a 100MB delta grew against {} ns over an empty delta",
            large.as_nanos(),
            trivial.as_nanos()
        );
    }
}
