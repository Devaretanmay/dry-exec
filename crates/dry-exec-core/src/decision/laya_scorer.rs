//! Optional local Laya scorer.
//!
//! This module never downloads weights. Callers provide a local checkpoint directory. Loading
//! failure belongs to the caller, which can pass `None` to retain deterministic scoring.

/// Strategy boundary used by the hybrid decision layer.
pub trait NeuralScoringStrategy {
    /// Return calibrated high-risk probability in `[0.0, 1.0]`.
    fn score(&self, task: &str, semantic_summary: &str) -> f32;
}

#[cfg(feature = "laya")]
mod local {
    use super::NeuralScoringStrategy;
    use laya::Agent;
    use serde_json::json;
    use std::path::Path;

    /// Local Laya checkpoint-backed scorer.
    pub struct LayaScorer {
        agent: Agent,
    }

    impl LayaScorer {
        /// Load an already-present checkpoint. No network access or implicit download occurs.
        pub fn load(model_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
            Ok(Self {
                agent: Agent::from_dir(model_dir)?,
            })
        }
    }

    impl NeuralScoringStrategy for LayaScorer {
        fn score(&self, task: &str, semantic_summary: &str) -> f32 {
            let state = json!({ "task": task, "proposed_action": semantic_summary });
            let questions = json!({
                "high_risk": {
                    "type": "noul",
                    "instructions": "Should this proposed action be escalated as high risk?",
                    "criteria": {
                        "true": "the action is high risk and should be escalated",
                        "false": "the action is not high risk"
                    }
                }
            });

            match self.agent.system_one(&state, &questions) {
                Ok(prediction) => prediction
                    .answer("high_risk")
                    .and_then(|answer| answer.noul)
                    .map(|noul| noul.clamp(0.0, 1.0))
                    .map(|noul| noul as f32)
                    .unwrap_or_else(|| {
                        eprintln!("local System-One scorer returned no noul probability");
                        0.0
                    }),
                Err(error) => {
                    eprintln!(
                        "local System-One inference failed; deterministic score retained: {error}"
                    );
                    0.0
                }
            }
        }
    }
}

#[cfg(feature = "laya")]
pub use local::LayaScorer;
