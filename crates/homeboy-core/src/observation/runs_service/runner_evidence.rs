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

use homeboy_lab_runner_contract::RunnerArtifactRef;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::observation::RunRecord;

/// A connected runner's status, slimmed to the fields `runs_service` needs.
#[derive(Debug, Clone, Default)]
pub struct RunnerConnectionInfo {
    pub runner_id: String,
    pub connected: bool,
    pub stale_runner_jobs: Vec<StaleRunnerJobInfo>,
    pub active_jobs: Vec<crate::api_jobs::ActiveRunnerJobSummary>,
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
    /// Release generation routing claims only after the durable observation
    /// lifecycle has removed the corresponding run or artifact provenance.
    /// Optional runner support keeps core retention generic and bounded.
    fn retire_durable_result_owner(
        &self,
        _run: &RunRecord,
        _artifact_id: Option<&str>,
    ) -> Result<()> {
        Ok(())
    }

    /// Mirror the run from a connected runner, if one owns it.
    fn mirror_connected_runner_run(&self, run_id: &str) -> Result<Option<RunRecord>>;

    /// Status of all known runners (connected or not).
    fn statuses(&self) -> Vec<RunnerConnectionInfo>;

    /// A latency-bounded, read-only view of each runner's active jobs — no
    /// generation reconcile, no per-generation network polling.
    ///
    /// `statuses()` reconciles the full generation ledger, which issues one
    /// blocking HTTP call per draining generation and can take minutes on a
    /// long-lived runner. Callers that only need the current/recent active-job
    /// view (e.g. `homeboy activity`) use this instead so latency stays bounded
    /// as generation history grows (#9522). The default falls back to
    /// `statuses()` for providers that have no cheaper path (tests, no-ops).
    fn statuses_indexed(&self) -> Vec<RunnerConnectionInfo> {
        self.statuses()
    }

    /// Raw GET against a runner's daemon API.
    fn daemon_api_get(&self, runner_id: &str, path: &str) -> Result<Value>;

    /// Fetch the content of an artifact from a connected runner's job.
    fn runner_artifact_content(
        &self,
        runner_id: &str,
        job_id: &str,
        artifact_id: &str,
    ) -> Result<Value>;

    /// Cancel a job running on a runner, returning the terminal job record and
    /// its events.
    fn runner_job_cancel(
        &self,
        runner_id: &str,
        job_id: &str,
    ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)>;

    /// Strictly cancel a daemon-local projection only when it remains bound to
    /// this runner and durable controller run.
    fn runner_job_cancel_projection(
        &self,
        _runner_id: &str,
        _job_id: &str,
        _durable_run_id: &str,
    ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
        Err(Error::validation_invalid_argument(
            "runner",
            "runner evidence provider does not support strict projection cancellation",
            None,
            None,
        ))
    }

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

    fn runner_artifact_content(
        &self,
        _runner_id: &str,
        _job_id: &str,
        _artifact_id: &str,
    ) -> Result<Value> {
        Err(Error::validation_invalid_argument(
            "runner",
            "no runner evidence provider is registered",
            None,
            None,
        ))
    }

    fn runner_job_cancel(
        &self,
        _runner_id: &str,
        _job_id: &str,
    ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
        Err(Error::validation_invalid_argument(
            "runner",
            "no runner evidence provider is registered",
            None,
            None,
        ))
    }

    fn runner_job_cancel_projection(
        &self,
        _runner_id: &str,
        _job_id: &str,
        _durable_run_id: &str,
    ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
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

/// Refresh runner-owned evidence and return its authenticated runner/job
/// identities without exposing runner-specific metadata to consumers.
pub fn mirrored_runner_job_identities(run_id: &str) -> Result<Vec<(String, String)>> {
    let mut identities = with_runner_evidence(|provider| -> Result<Vec<(String, String)>> {
        Ok(provider
            .refresh_mirrored_daemon_evidence(run_id)?
            .unwrap_or_default()
            .iter()
            .filter_map(|run| provider.mirrored_runner_job_identity(run))
            .collect())
    })?;
    identities.sort();
    identities.dedup();
    Ok(identities)
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
pub fn with_runner_evidence<T>(f: impl FnOnce(&dyn RunnerEvidenceProvider) -> T) -> T {
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
    use std::sync::{Arc, OnceLock};

    use crate::observation::{ObservationStore, RunStatus};
    use crate::test_support::with_isolated_home;

    fn provider_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn run(id: &str) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            kind: "agent-task".to_string(),
            started_at: "2026-07-16T00:00:00Z".to_string(),
            status: "running".to_string(),
            ..Default::default()
        }
    }

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
        let _lock = provider_lock().lock().expect("provider lock");
        struct FakeProvider;
        impl RunnerEvidenceProvider for FakeProvider {
            fn mirror_connected_runner_run(&self, _: &str) -> Result<Option<RunRecord>> {
                Ok(None)
            }
            fn statuses(&self) -> Vec<RunnerConnectionInfo> {
                vec![RunnerConnectionInfo {
                    runner_id: "fake".to_string(),
                    connected: true,
                    active_jobs: Vec::new(),
                    stale_runner_jobs: Vec::new(),
                }]
            }
            fn daemon_api_get(&self, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_artifact_content(&self, _: &str, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_job_cancel(
                &self,
                _: &str,
                _: &str,
            ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
                unreachable!()
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

    #[test]
    fn statuses_indexed_defaults_to_statuses_and_honors_override() {
        // Default: a provider with no cheaper path serves statuses_indexed from
        // statuses (so `homeboy activity` still works for every provider).
        struct DefaultProvider;
        impl RunnerEvidenceProvider for DefaultProvider {
            fn mirror_connected_runner_run(&self, _: &str) -> Result<Option<RunRecord>> {
                Ok(None)
            }
            fn statuses(&self) -> Vec<RunnerConnectionInfo> {
                vec![RunnerConnectionInfo {
                    runner_id: "from-statuses".to_string(),
                    connected: true,
                    active_jobs: Vec::new(),
                    stale_runner_jobs: Vec::new(),
                }]
            }
            fn daemon_api_get(&self, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_artifact_content(&self, _: &str, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_job_cancel(
                &self,
                _: &str,
                _: &str,
            ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
                unreachable!()
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
        let default = DefaultProvider;
        assert_eq!(default.statuses_indexed().len(), 1);
        assert_eq!(default.statuses_indexed()[0].runner_id, "from-statuses");

        // Override: a provider with a cheaper indexed path uses it, NOT statuses.
        // (statuses() here would panic — proving activity never calls it.)
        struct IndexedProvider;
        impl RunnerEvidenceProvider for IndexedProvider {
            fn mirror_connected_runner_run(&self, _: &str) -> Result<Option<RunRecord>> {
                Ok(None)
            }
            fn statuses(&self) -> Vec<RunnerConnectionInfo> {
                panic!("statuses() must not be called on the indexed activity path");
            }
            fn statuses_indexed(&self) -> Vec<RunnerConnectionInfo> {
                vec![RunnerConnectionInfo {
                    runner_id: "from-indexed".to_string(),
                    connected: true,
                    active_jobs: Vec::new(),
                    stale_runner_jobs: Vec::new(),
                }]
            }
            fn daemon_api_get(&self, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_artifact_content(&self, _: &str, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_job_cancel(
                &self,
                _: &str,
                _: &str,
            ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
                unreachable!()
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
        let indexed = IndexedProvider;
        assert_eq!(indexed.statuses_indexed().len(), 1);
        assert_eq!(indexed.statuses_indexed()[0].runner_id, "from-indexed");
    }

    #[test]
    fn mirrored_runner_job_identities_deduplicates_and_preserves_ambiguity() {
        let _lock = provider_lock().lock().expect("provider lock");
        struct FakeProvider;

        impl RunnerEvidenceProvider for FakeProvider {
            fn mirror_connected_runner_run(&self, _: &str) -> Result<Option<RunRecord>> {
                Ok(None)
            }
            fn statuses(&self) -> Vec<RunnerConnectionInfo> {
                Vec::new()
            }
            fn daemon_api_get(&self, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_artifact_content(&self, _: &str, _: &str, _: &str) -> Result<Value> {
                Ok(Value::Null)
            }
            fn runner_job_cancel(
                &self,
                _: &str,
                _: &str,
            ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
                unreachable!()
            }
            fn refresh_mirrored_daemon_evidence(
                &self,
                run_id: &str,
            ) -> Result<Option<Vec<RunRecord>>> {
                Ok(Some(match run_id {
                    "duplicates" => vec![run("same"), run("same")],
                    "none" => vec![run("no-identity")],
                    "distinct" => vec![run("first"), run("second")],
                    _ => Vec::new(),
                }))
            }
            fn mirrored_runner_job_identity(&self, run: &RunRecord) -> Option<(String, String)> {
                match run.id.as_str() {
                    "same" => Some(("runner".to_string(), "job".to_string())),
                    "first" => Some(("runner-a".to_string(), "job-a".to_string())),
                    "second" => Some(("runner-b".to_string(), "job-b".to_string())),
                    _ => None,
                }
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
        assert_eq!(
            mirrored_runner_job_identities("duplicates").expect("duplicate identities"),
            vec![("runner".to_string(), "job".to_string())]
        );
        assert!(mirrored_runner_job_identities("none")
            .expect("no identity")
            .is_empty());
        assert_eq!(
            mirrored_runner_job_identities("distinct").expect("distinct identities"),
            vec![
                ("runner-a".to_string(), "job-a".to_string()),
                ("runner-b".to_string(), "job-b".to_string()),
            ]
        );

        PROVIDER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    struct SelectedRefreshProvider {
        calls: Arc<Mutex<Vec<String>>>,
        error: Option<Error>,
    }

    impl RunnerEvidenceProvider for SelectedRefreshProvider {
        fn mirror_connected_runner_run(&self, _: &str) -> Result<Option<RunRecord>> {
            Ok(None)
        }

        fn statuses(&self) -> Vec<RunnerConnectionInfo> {
            Vec::new()
        }

        fn daemon_api_get(&self, _: &str, _: &str) -> Result<Value> {
            Ok(Value::Null)
        }

        fn runner_artifact_content(&self, _: &str, _: &str, _: &str) -> Result<Value> {
            Ok(Value::Null)
        }

        fn runner_job_cancel(
            &self,
            _: &str,
            _: &str,
        ) -> Result<(crate::api_jobs::Job, Vec<crate::api_jobs::JobEvent>)> {
            unreachable!()
        }

        fn refresh_mirrored_daemon_evidence(&self, run_id: &str) -> Result<Option<Vec<RunRecord>>> {
            self.calls.lock().expect("calls").push(run_id.to_string());
            self.error.clone().map_or(Ok(None), Err)
        }

        fn mirrored_runner_job_identity(&self, run: &RunRecord) -> Option<(String, String)> {
            run.metadata_json
                .pointer("/lab/runner/id")
                .and_then(Value::as_str)
                .zip(
                    run.metadata_json
                        .pointer("/lab/remote_job/id")
                        .and_then(Value::as_str),
                )
                .map(|(runner, job)| (runner.to_string(), job.to_string()))
        }

        fn download_remote_artifact(
            &self,
            _: &str,
            _: Option<PathBuf>,
        ) -> Result<RemoteArtifactDownloadInfo> {
            unreachable!()
        }
    }

    fn mirrored_run(id: &str) -> RunRecord {
        let mut run = run(id);
        run.metadata_json = serde_json::json!({
            "lab": { "runner": { "id": "lab" }, "remote_job": { "id": "job-1" } }
        });
        run
    }

    #[test]
    fn selected_refresh_skips_unrelated_mirrors_and_non_mirrors() {
        let _lock = provider_lock().lock().expect("provider lock");
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let selected = run("selected");
            let unrelated = mirrored_run("unrelated");
            store.import_run(&selected).expect("selected");
            store.import_run(&unrelated).expect("unrelated");
            let calls = Arc::new(Mutex::new(Vec::new()));
            register_runner_evidence_provider(Box::new(SelectedRefreshProvider {
                calls: calls.clone(),
                error: Some(Error::internal_unexpected("unrelated refresh must not run")),
            }));

            assert!(
                super::super::refresh_selected_mirrored_daemon_evidence(&store, &selected)
                    .is_none()
            );
            assert!(calls.lock().expect("calls").is_empty());
        });
        PROVIDER.lock().expect("provider").take();
    }

    #[test]
    fn selected_daemon_job_not_found_becomes_stale_diagnostics() {
        let _lock = provider_lock().lock().expect("provider lock");
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let selected = mirrored_run("selected");
            store.import_run(&selected).expect("selected");
            let calls = Arc::new(Mutex::new(Vec::new()));
            register_runner_evidence_provider(Box::new(SelectedRefreshProvider {
                calls: calls.clone(),
                error: Some(Error::new(
                    crate::error::ErrorCode::InternalUnexpected,
                    "daemon request failed: job not found",
                    serde_json::json!({ "http_status": 404, "path": "/jobs/job-1" }),
                )),
            }));

            assert!(
                super::super::refresh_selected_mirrored_daemon_evidence(&store, &selected)
                    .is_none()
            );
            assert_eq!(*calls.lock().expect("calls"), vec!["selected"]);
            let refreshed = store.get_run("selected").expect("read").expect("run");
            assert_eq!(refreshed.status, RunStatus::Stale.as_str());
            assert_eq!(
                refreshed.metadata_json["runner_terminal_evidence"]["stale_reason"],
                "daemon_job_not_found"
            );
            assert_eq!(
                refreshed.metadata_json["runner_terminal_evidence"]["diagnostic"]["details"]
                    ["http_status"],
                404
            );
        });
        PROVIDER.lock().expect("provider").take();
    }

    #[test]
    fn selected_transport_refresh_failure_remains_actionable() {
        let _lock = provider_lock().lock().expect("provider lock");
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let selected = mirrored_run("selected");
            store.import_run(&selected).expect("selected");
            let calls = Arc::new(Mutex::new(Vec::new()));
            register_runner_evidence_provider(Box::new(SelectedRefreshProvider {
                calls: calls.clone(),
                error: Some(Error::internal_unexpected("runner transport unavailable")),
            }));

            let err = super::super::refresh_selected_mirrored_daemon_evidence(&store, &selected)
                .expect("actionable refresh error");
            assert_eq!(err.message, "runner transport unavailable");
            assert_eq!(*calls.lock().expect("calls"), vec!["selected"]);
            assert_eq!(
                store
                    .get_run("selected")
                    .expect("read")
                    .expect("run")
                    .status,
                RunStatus::Running.as_str()
            );
        });
        PROVIDER.lock().expect("provider").take();
    }
}
