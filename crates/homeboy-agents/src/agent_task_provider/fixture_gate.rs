//! Compile-time gate for the `fixture` test-double backend.
//!
//! `fixture` is not an agent runtime. It has no provider manifest under
//! the extension directory, it is never discovered by the extension
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

use super::{AgentTaskExecutorRequest, AgentTaskOutcome};

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
pub(crate) fn fixture_provider_outcome(
    request: &AgentTaskExecutorRequest,
) -> Option<AgentTaskOutcome> {
    is_fixture_backend(&request.request.executor.backend).then(|| {
        super::fixtures::run_fixture_provider(
            &request.request,
            &request.artifacts_path,
            &request.artifacts_path_provenance,
        )
    })
}

/// Run the deterministic test double when the request selects it.
///
/// Returns `None` in a production build, so the caller falls through to real
/// provider resolution unconditionally.
#[cfg(not(any(test, feature = "test-support")))]
pub(crate) fn fixture_provider_outcome(
    _request: &AgentTaskExecutorRequest,
) -> Option<AgentTaskOutcome> {
    None
}
