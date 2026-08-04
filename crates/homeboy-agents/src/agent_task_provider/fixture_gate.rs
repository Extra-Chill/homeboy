//! Compile-time gate for the `fixture` test-double backend.
//!
//! `fixture` is not an agent runtime. It has no provider manifest under
//! `_extensions/agent-runtimes/`, it is never discovered by the extension
//! catalog, and it cannot be resolved by `resolve_provider_for_backend`. It
//! exists only so tests can dispatch a deterministic executor that writes
//! predictable artifacts without credentials or an external process.
//!
//! Every branch on that name therefore lives behind `test-support` (or an
//! in-crate `cfg(test)` build). The shipped binary contains no fixture
//! short-circuit ahead of provider resolution: `is_fixture_backend` folds to a
//! constant `false` and `fixture_provider_outcome` folds to a constant `None`,
//! so a release build routes `--backend fixture` through the same data-driven
//! resolution path as any other unknown backend and fails with the normal
//! `ProviderResolution` diagnostic.
//!
//! Consumers enable the feature as a dev-dependency:
//! `homeboy-agents = { path = "...", features = ["test-support"] }`.

use super::{AgentTaskOutcome, AgentTaskRequest};

/// Backend identifier reserved for the in-tree test double.
#[cfg(any(test, feature = "test-support"))]
pub(crate) const FIXTURE_BACKEND: &str = "fixture";

/// Whether `backend` names the in-tree test double.
///
/// Always `false` in a production build — the fixture backend does not exist
/// there.
#[cfg(any(test, feature = "test-support"))]
pub fn is_fixture_backend(backend: &str) -> bool {
    backend == FIXTURE_BACKEND
}

/// Whether `backend` names the in-tree test double.
///
/// Always `false` in a production build — the fixture backend does not exist
/// there.
#[cfg(not(any(test, feature = "test-support")))]
pub fn is_fixture_backend(_backend: &str) -> bool {
    false
}

/// Run the deterministic test double when the request selects it.
///
/// Returns `None` in a production build, so the caller falls through to real
/// provider resolution unconditionally.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn fixture_provider_outcome(request: &AgentTaskRequest) -> Option<AgentTaskOutcome> {
    is_fixture_backend(&request.executor.backend)
        .then(|| super::fixtures::run_fixture_provider(request, &request.artifacts_path))
}

/// Run the deterministic test double when the request selects it.
///
/// Returns `None` in a production build, so the caller falls through to real
/// provider resolution unconditionally.
#[cfg(not(any(test, feature = "test-support")))]
pub(crate) fn fixture_provider_outcome(_request: &AgentTaskRequest) -> Option<AgentTaskOutcome> {
    None
}

#[cfg(test)]
mod tests {
    use super::is_fixture_backend;

    #[test]
    fn only_the_exact_fixture_backend_name_selects_the_test_double() {
        assert!(is_fixture_backend("fixture"));
        assert!(!is_fixture_backend("fixtures"));
        assert!(!is_fixture_backend("Fixture"));
        assert!(!is_fixture_backend(""));
    }

    #[test]
    fn the_provider_executor_never_branches_on_the_backend_name_by_string() {
        // The shipped executor must reach provider resolution for every
        // backend. A string literal naming the test double there would put a
        // test-only branch in the release binary, ahead of every real provider
        // (#11118). This gate module is the only place the name may appear.
        //
        // The needle is assembled rather than written out so this assertion
        // does not match its own source file.
        let needle = format!(
            "{quote}{name}{quote}",
            quote = '"',
            name = super::FIXTURE_BACKEND
        );
        assert!(
            !include_str!("executor.rs").contains(&needle),
            "agent_task_provider::executor must not name the test-double backend; \
             route the decision through fixture_gate::fixture_provider_outcome instead"
        );
    }
}
