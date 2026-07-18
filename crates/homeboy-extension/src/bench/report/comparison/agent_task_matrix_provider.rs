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
use std::sync::Mutex;

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

fn provider_slot() -> &'static Mutex<Option<Box<dyn BenchAgentTaskMatrixProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn BenchAgentTaskMatrixProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the bench agent-task matrix provider. Called once at startup by the
/// agent-task layer.
pub fn register_bench_agent_task_matrix_provider(provider: Box<dyn BenchAgentTaskMatrixProvider>) {
    let mut slot = provider_slot()
        .lock()
        .expect("bench agent-task matrix provider lock");
    *slot = Some(provider);
}

/// The agent-task matrix (plan + aggregate as JSON) for a bench comparison, via
/// the registered provider (or none when the agent-task subsystem is absent).
pub(crate) fn bench_agent_task_matrix(
    component: &str,
    iterations: u64,
    entries: &[RigBenchEntry],
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> Option<(Value, Value)> {
    let slot = provider_slot()
        .lock()
        .expect("bench agent-task matrix provider lock");
    match slot.as_deref() {
        Some(provider) => {
            provider.bench_agent_task_matrix(component, iterations, entries, axes_by_rig)
        }
        None => NoopProvider.bench_agent_task_matrix(component, iterations, entries, axes_by_rig),
    }
}
