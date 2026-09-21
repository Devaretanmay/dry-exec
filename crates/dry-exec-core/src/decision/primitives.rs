//! System-One decision primitives: categorical choice, calibrated score, and escalation state.

/// Categorical validation of a state delta against environment rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// No categorical refusal and no calibrated mutation ceiling breached.
    Allowed,

    /// The transparent proxy refused a route outside the registered schema.
    Blocked,

    /// A calibrated mutation ceiling was breached.
    Violated,
}

impl Choice {
    /// Stable wire representation shared with the Python SDK.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
            Self::Violated => "violated",
        }
    }
}

/// Calibrated risk metric, clamped strictly into `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Score(f32);

impl Score {
    /// Lower bound of the calibrated range.
    pub const MIN: f32 = 0.0;

    /// Upper bound of the calibrated range.
    pub const MAX: f32 = 1.0;

    /// Clamps a raw ratio sum into the calibrated range.
    ///
    /// Non-finite input saturates to [`Score::MAX`]: an unevaluable ratio is routed as
    /// maximum risk rather than as an unconstrained value.
    pub fn new(raw: f32) -> Self {
        if raw.is_nan() {
            return Self(Self::MAX);
        }
        Self(raw.clamp(Self::MIN, Self::MAX))
    }

    /// Scalar value of the calibrated metric.
    pub fn value(self) -> f32 {
        self.0
    }
}

impl Default for Score {
    fn default() -> Self {
        Self(Self::MIN)
    }
}

/// Escalation state routed from the System-One decision layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noul {
    /// Within calibrated limits; the autonomous execution loop may proceed.
    AutoCommit,

    /// Escalation required: the mutation needs explicit approval before it is applied.
    Escalate,
}

impl Noul {
    /// Stable wire representation shared with the Python SDK.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoCommit => "auto_commit",
            Self::Escalate => "escalate",
        }
    }
}

/// Deterministic System-One decision receipt for a single state delta.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionReceipt {
    /// Categorical validation outcome.
    pub choice: Choice,

    /// Calibrated risk metric in `[0.0, 1.0]`.
    pub risk_score: Score,

    /// Escalation routing derived from the choice and the calibrated risk threshold.
    pub noul_trigger: Noul,

    /// Interpolated system reason; never generated text.
    pub reason: String,
}
