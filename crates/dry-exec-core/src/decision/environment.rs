//! Calibrated decision thresholds and the scalar state delta summary they are evaluated against.

use crate::delta::StateDelta;

/// Scalar summary of a state delta: the sole input the decision layer reads.
///
/// The summary is `Copy` and holds counts and totals only, so the scorer cannot reach the
/// mutation vectors: the decision layer is O(1) in the size of the state by construction,
/// and the projection itself reads only lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeltaSummary {
    /// Aggregate count of mutated bytes across memory, filesystem, and network.
    pub total_bytes_mutated: usize,

    /// Number of mutated memory pages.
    pub memory_pages_mutated: usize,

    /// Number of ephemeral filesystem mutations.
    pub fs_mutations: usize,

    /// Number of outbound requests intercepted by the transparent proxy.
    pub network_calls: usize,

    /// Number of intercepted routes the transparent proxy refused outside the schema.
    pub schema_breaches: usize,

    /// State delta computation latency in nanoseconds.
    pub duration_nanos: u64,
}

impl DeltaSummary {
    /// Projects a state delta onto its scalar summary in O(1) time.
    pub fn from_state_delta(delta: &StateDelta) -> Self {
        Self {
            total_bytes_mutated: delta.total_bytes_mutated,
            memory_pages_mutated: delta.memory_mutations.len(),
            fs_mutations: delta.fs_mutations.len(),
            network_calls: delta.network_mutations.len(),
            schema_breaches: delta.schema_breaches,
            duration_nanos: delta.duration_nanos,
        }
    }
}

impl From<&StateDelta> for DeltaSummary {
    fn from(delta: &StateDelta) -> Self {
        Self::from_state_delta(delta)
    }
}

/// Calibrated decision thresholds governing the System-One routing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Environment {
    /// Mutation volume ceiling driving the byte term of the risk score.
    pub max_mutated_bytes: usize,

    /// Intercepted call ceiling driving the network term of the risk score.
    pub max_network_calls: usize,

    /// Calibrated risk ceiling; a score above it requires escalation.
    pub max_risk_threshold: f32,
}

impl Default for Environment {
    /// Calibrated defaults, mirrored by the Python `Environment` schema.
    fn default() -> Self {
        Self {
            max_mutated_bytes: 1_048_576,
            max_network_calls: 16,
            max_risk_threshold: 0.5,
        }
    }
}
