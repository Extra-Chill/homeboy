//! Bench agent-task matrix hook.
//!
//! Cross-rig bench comparison can project its rig entries into an agent-task
//! matrix (a plan + aggregate) so bench results can be reviewed with the same
//! matrix tooling as agent-task runs. Building that matrix uses agent-task types
//! and expansion, so it is inverted behind this provider: bench owns the
//! comparison report, the agent-task layer builds the matrix from bench inputs.
//!
//! With no provider registered (no agent-task subsystem present) the no-op
//! provider produces no matrix, and the comparison report simply omits it.

use std::collections::BTreeMap;

use serde_json::Value;

use super::types::RigBenchEntry;

/// Builds the agent-task matrix (plan + aggregate, as JSON) for a cross-rig
/// bench comparison.
pub trait BenchAgentTaskMatrixProvider: Send + Sync {
    /// Project the bench entries into an agent-task matrix plan + aggregate,
    /// returned as JSON. `None` when no matrix can be built.
    fn bench_agent_task_matrix(
        &self,
        component: &str,
        iterations: u64,
        entries: &[RigBenchEntry],
        axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Option<(Value, Value)>;
}

struct NoopProvider;

impl BenchAgentTaskMatrixProvider for NoopProvider {
    fn bench_agent_task_matrix(
        &self,
        _component: &str,
        _iterations: u64,
        _entries: &[RigBenchEntry],
        _axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Option<(Value, Value)> {
        None
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn BenchAgentTaskMatrixProvider,
    noop: NoopProvider,
    /// Register the bench agent-task matrix provider. Called once at startup by the
    /// agent-task layer.
    register: pub fn register_bench_agent_task_matrix_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

/// The agent-task matrix (plan + aggregate as JSON) for a bench comparison, via
/// the registered provider (or none when the agent-task subsystem is absent).
pub(crate) fn bench_agent_task_matrix(
    component: &str,
    iterations: u64,
    entries: &[RigBenchEntry],
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> Option<(Value, Value)> {
    with_provider(|p| p.bench_agent_task_matrix(component, iterations, entries, axes_by_rig))
}
