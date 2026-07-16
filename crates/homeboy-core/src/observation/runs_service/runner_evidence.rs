//! Runner-evidence provider hook.
//!
//! `runs_service` enriches run/artifact lookups with live runner + daemon
//! evidence (mirrored runs, connected-runner status, remote artifact
//! downloads). Runner is an optional Lab-offload feature, so core must not
//! depend on runner *behavior* to do this. Instead, core defines the
//! [`RunnerEvidenceProvider`] contract here and the runner layer registers an
//! implementation at startup. When no provider is registered (runner absent),
//! the [`NoopRunnerEvidenceProvider`] returns empty evidence — which is exactly
//! how the callers already behave when no runner is connected.

use std::path::PathBuf;
use std::sync::Mutex;

use homeboy_runner_contract::RunnerArtifactRef;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::observation::RunRecord;

/// A connected runner's status, slimmed to the fields `runs_service` needs.
#[derive(Debug, Clone, Default)]
pub struct RunnerConnectionInfo {
    pub runner_id: String,
    pub connected: bool,
    pub stale_runner_jobs: Vec<StaleRunnerJobInfo>,
}

/// A stale runner job, slimmed to the fields `runs_service` reconciles. Field
/// types mirror the runner-side job record so the reconciled `runner_terminal
/// _evidence` JSON is byte-for-byte identical to the pre-hook behavior.
#[derive(Debug, Clone, Default)]
pub struct StaleRunnerJobInfo {
    pub durable_run_id: Option<String>,
    pub runner_id: String,
    pub job_id: String,
    pub status: String,
    pub lifecycle_state: Option<String>,
    pub stale_reason: Option<String>,
    pub retryable: Option<bool>,
}

/// The result of downloading a remote runner artifact, slimmed to what
/// `runs_service` surfaces in an `ArtifactFetchOutcome`.
#[derive(Debug)]
pub struct RemoteArtifactDownloadInfo {
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub artifact_ref: RunnerArtifactRef,
}

/// The runner-evidence contract `runs_service` depends on. Implemented by the
/// runner layer and registered at startup; core calls it without depending on
/// runner behavior.
pub trait RunnerEvidenceProvider: Send + Sync {
    /// Mirror the run from a connected runner, if one owns it.
    fn mirror_connected_runner_run(&self, run_id: &str) -> Result<Option<RunRecord>>;

    /// Status of all known runners (connected or not).
    fn statuses(&self) -> Vec<RunnerConnectionInfo>;

    /// Raw GET against a runner's daemon API.
    fn daemon_api_get(&self, runner_id: &str, path: &str) -> Result<Value>;

    /// Refresh mirrored daemon evidence for a run, returning any mirrored runs.
    fn refresh_mirrored_daemon_evidence(&self, run_id: &str) -> Result<Option<Vec<RunRecord>>>;

    /// The `(runner_id, job_id)` identity mirrored onto a run record, if any.
    fn mirrored_runner_job_identity(&self, run: &RunRecord) -> Option<(String, String)>;

    /// Download a remote runner artifact to `output` (or a temp path).
    fn download_remote_artifact(
        &self,
        path: &str,
        output: Option<PathBuf>,
    ) -> Result<RemoteArtifactDownloadInfo>;
}

/// Default provider used when no runner layer is registered. Returns empty
/// evidence for the best-effort methods; the one non-degradable method
/// (`download_remote_artifact`) errors clearly, since a remote artifact cannot
/// be resolved without a runner.
struct NoopRunnerEvidenceProvider;

impl RunnerEvidenceProvider for NoopRunnerEvidenceProvider {
    fn mirror_connected_runner_run(&self, _run_id: &str) -> Result<Option<RunRecord>> {
        Ok(None)
    }

    fn statuses(&self) -> Vec<RunnerConnectionInfo> {
        Vec::new()
    }

    fn daemon_api_get(&self, _runner_id: &str, _path: &str) -> Result<Value> {
        Err(Error::validation_invalid_argument(
            "runner",
            "no runner evidence provider is registered",
            None,
            None,
        ))
    }

    fn refresh_mirrored_daemon_evidence(&self, _run_id: &str) -> Result<Option<Vec<RunRecord>>> {
        Ok(None)
    }

    fn mirrored_runner_job_identity(&self, _run: &RunRecord) -> Option<(String, String)> {
        None
    }

    fn download_remote_artifact(
        &self,
        _path: &str,
        _output: Option<PathBuf>,
    ) -> Result<RemoteArtifactDownloadInfo> {
        Err(Error::validation_invalid_argument(
            "artifact",
            "cannot download a remote runner artifact without a registered runner evidence provider",
            None,
            None,
        ))
    }
}

static PROVIDER: Mutex<Option<Box<dyn RunnerEvidenceProvider>>> = Mutex::new(None);

/// Register the runner-evidence provider. Called once at binary startup by the
/// runner layer (via the CLI). Replaces any previously registered provider.
pub fn register_runner_evidence_provider(provider: Box<dyn RunnerEvidenceProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Whether a runner-evidence provider is currently registered.
pub fn has_runner_evidence_provider() -> bool {
    PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

/// Run `f` against the registered provider, or the no-op provider if none is
/// registered. Keeps the lock held only for the duration of the call.
pub(crate) fn with_runner_evidence<T>(f: impl FnOnce(&dyn RunnerEvidenceProvider) -> T) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopRunnerEvidenceProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards graceful degradation: the best-effort evidence methods must return
    /// empty (not error, not panic) when no provider is registered. runs_service
    /// callers already treat "no runner connected" as empty; the no-op provider
    /// must preserve that so run-lookup keeps working with runner absent.
    #[test]
    fn noop_provider_degrades_gracefully() {
        let noop = NoopRunnerEvidenceProvider;
        assert!(noop.statuses().is_empty());
        assert!(noop.mirror_connected_runner_run("run-x").unwrap().is_none());
        assert!(noop
            .refresh_mirrored_daemon_evidence("run-x")
            .unwrap()
            .is_none());
    }

    /// The one non-degradable method (a remote artifact genuinely can't be
    /// resolved without a runner) must surface a clear error rather than a
    /// silent empty result.
    #[test]
    fn noop_provider_download_errors_clearly() {
        let noop = NoopRunnerEvidenceProvider;
        let err = noop
            .download_remote_artifact("runner-artifact://x", None)
            .expect_err("download must error without a provider");
        assert!(err.message.contains("runner evidence provider"));
    }

    /// A registered provider is used in place of the no-op default.
    #[test]
    fn registered_provider_is_used() {
        struct FakeProvider;
        impl RunnerEvidenceProvider for FakeProvider {
            fn mirror_connected_runner_run(&self, _: &str) -> Result<Option<RunRecord>> {
                Ok(None)
            }
            fn statuses(&self) -> Vec<RunnerConnectionInfo> {
                vec![RunnerConnectionInfo {
                    runner_id: "fake".to_string(),
                    connected: true,
                    stale_runner_jobs: Vec::new(),
                }]
            }
            fn daemon_api_get(&self, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn refresh_mirrored_daemon_evidence(&self, _: &str) -> Result<Option<Vec<RunRecord>>> {
                Ok(None)
            }
            fn mirrored_runner_job_identity(&self, _: &RunRecord) -> Option<(String, String)> {
                None
            }
            fn download_remote_artifact(
                &self,
                _: &str,
                _: Option<PathBuf>,
            ) -> Result<RemoteArtifactDownloadInfo> {
                unreachable!()
            }
        }

        register_runner_evidence_provider(Box::new(FakeProvider));
        let statuses = with_runner_evidence(|p| p.statuses());
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].runner_id, "fake");

        // Reset so the registered fake doesn't leak into other tests sharing the
        // process-global provider.
        PROVIDER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }
}
