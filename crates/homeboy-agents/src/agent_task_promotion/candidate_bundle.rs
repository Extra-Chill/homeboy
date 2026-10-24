//! Immutable candidate transport; never falls back to patch evidence.
use crate::agent_task::AgentTaskCandidate;
use homeboy_core::{Error, Result};
use std::path::Path;
pub const CANDIDATE_BUNDLE_ARTIFACT_KIND: &str = "git_bundle";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateBundleError {
    pub run_id: String,
    pub kind: &'static str,
    pub expected: String,
    pub observed: String,
}
impl std::fmt::Display for CandidateBundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "non-recoverable: run {} candidate bundle {}: expected {}, observed {}",
            self.run_id, self.kind, self.expected, self.observed
        )
    }
}
impl std::error::Error for CandidateBundleError {}
impl From<CandidateBundleError> for Error {
    fn from(error: CandidateBundleError) -> Self {
        Error::new(homeboy_core::ErrorCode::ValidationInvalidArgument, error.to_string(), serde_json::json!({"field":"source.candidate", "run_id":error.run_id, "kind":error.kind, "expected":error.expected, "observed":error.observed})).with_retryable(false).with_source(error)
    }
}
fn bundle_error(run_id: &str, kind: &'static str, expected: &str, observed: String) -> Error {
    CandidateBundleError {
        run_id: run_id.into(),
        kind,
        expected: expected.into(),
        observed,
    }
    .into()
}
use super::promote::git_output as git;

pub fn fetch_candidate_bundle(
    repo: &Path,
    bundle: &Path,
    run_id: &str,
    candidate: &AgentTaskCandidate,
) -> Result<String> {
    let reference = format!(
        "refs/homeboy/candidates/{}",
        homeboy_core::paths::sanitize_path_segment(run_id)
    );
    let missing = |error: Error| {
        bundle_error(
            run_id,
            "missing_object",
            &candidate.candidate_sha,
            error.to_string(),
        )
    };
    let bundle = bundle.to_string_lossy();
    git(repo, &["bundle", "verify", "--quiet", &bundle]).map_err(missing)?;
    git(
        repo,
        &[
            "fetch",
            "--no-tags",
            "--quiet",
            &bundle,
            &format!("+{reference}:{reference}"),
        ],
    )
    .map_err(missing)?;
    let verified = verify_candidate_ref(repo, run_id, candidate);
    if verified.is_err() {
        let _ = git(repo, &["update-ref", "-d", &reference]);
    }
    verified.map(|_| reference)
}
pub fn verify_candidate_ref(
    repo: &Path,
    run_id: &str,
    candidate: &AgentTaskCandidate,
) -> Result<()> {
    let reference = format!(
        "refs/homeboy/candidates/{}",
        homeboy_core::paths::sanitize_path_segment(run_id)
    );
    let missing = |error: Error| {
        bundle_error(
            run_id,
            "missing_object",
            &candidate.candidate_sha,
            error.to_string(),
        )
    };
    let observed = git(repo, &["rev-parse", "--verify", &reference]).map_err(missing)?;
    if observed != candidate.candidate_sha {
        return Err(bundle_error(
            run_id,
            "commit_mismatch",
            &candidate.candidate_sha,
            observed,
        ));
    }
    let observed = git(repo, &["rev-parse", &format!("{reference}^{{tree}}")]).map_err(missing)?;
    if observed != candidate.tree_sha {
        return Err(bundle_error(
            run_id,
            "tree_mismatch",
            &candidate.tree_sha,
            observed,
        ));
    }
    Ok(())
}

pub(crate) fn validate_destination(repo: &Path, candidate: &AgentTaskCandidate) -> Result<()> {
    super::validate_candidate_commit(repo, candidate)?;
    let head = git(repo, &["rev-parse", "HEAD"])?;
    if head != candidate.base_sha {
        return Err(super::promote::CandidateDestinationMoved {
            expected: candidate.base_sha.clone(),
            actual: head,
        }
        .into());
    }
    if !git(repo, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            "candidate destination must be clean",
            None,
            None,
        ));
    }
    Ok(())
}
pub fn materialize_candidate_tree(repo: &Path, candidate: &AgentTaskCandidate) -> Result<()> {
    validate_destination(repo, candidate)?;
    git(
        repo,
        &["read-tree", "-u", "-m", "HEAD", &candidate.candidate_sha],
    )?;
    Ok(())
}

pub(crate) fn fetch_run_candidates(
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
    plan: &crate::agent_tasks::scheduler::AgentTaskPlan,
    aggregate: &crate::agent_tasks::scheduler::AgentTaskAggregate,
) -> Result<()> {
    // Telemetry setup must not prevent candidate recovery.
    let store = match lifecycle_store.open_observation_initialized() {
        Ok(store) => Some(store),
        Err(error) => {
            homeboy_core::log_status!("observe", "candidate timing persistence failed: {}", error);
            None
        }
    };
    let operation = || fetch_run_candidates_internal(record, plan, aggregate);
    match store {
        Some(store) => {
            // The remote observation ends at Lab terminal status, not when the
            // controller finally picks up the candidate. Attribute that delay
            // separately from acquisition work.
            let wait = (|| -> Result<()> {
                let spans = store.phase_spans(&record.run_id)?;
                if spans.span("promote.acquire").is_some() {
                    return Ok(());
                }
                let event_terminal = record
                    .metadata
                    .get("runner_job_events")
                    .and_then(|events| {
                        serde_json::from_value::<Vec<homeboy_core::api_jobs::JobEvent>>(
                            events.clone(),
                        )
                        .ok()
                    })
                    .and_then(|events| {
                        events
                            .into_iter()
                            .filter(|event| {
                                event.kind == homeboy_core::api_jobs::JobEventKind::Result
                            })
                            .map(|event| event.timestamp_ms)
                            .max()
                    })
                    .and_then(|ms| i64::try_from(ms).ok())
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|time| time.fixed_offset());
                let terminal = event_terminal.or_else(|| {
                    spans
                        .spans
                        .iter()
                        .filter(|span| span.id == "stage.lab.observe")
                        .filter_map(|span| {
                            let start =
                                chrono::DateTime::parse_from_rfc3339(&span.started_at).ok()?;
                            let duration = span.duration_ms?;
                            Some(start + chrono::Duration::milliseconds(duration as i64))
                        })
                        .max()
                });
                if let Some(terminal) = terminal {
                    store.finish_run_phase(
                        &record.run_id,
                        "promote.wait",
                        &terminal.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        false,
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = wait {
                homeboy_core::log_status!("observe", "pickup timing persistence failed: {}", error);
            }
            store.time_run_phase(&record.run_id, "promote.acquire", operation)
        }
        None => operation(),
    }
}

fn fetch_run_candidates_internal(
    record: &crate::agent_task_lifecycle::AgentTaskRunRecord,
    plan: &crate::agent_tasks::scheduler::AgentTaskPlan,
    aggregate: &crate::agent_tasks::scheduler::AgentTaskAggregate,
) -> Result<()> {
    for outcome in &aggregate.outcomes {
        let Some(candidate) = &outcome.candidate else {
            continue;
        };
        let unavailable = |observed| {
            bundle_error(
                &record.run_id,
                "missing_object",
                &candidate.candidate_sha,
                observed,
            )
        };
        let mut artifact = outcome
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == CANDIDATE_BUNDLE_ARTIFACT_KIND)
            .cloned()
            .ok_or_else(|| unavailable("missing candidate bundle".into()))?;
        crate::agent_task_lifecycle::artifact_materialization::materialize_artifact(
            &mut artifact,
            &record.run_id,
            &outcome.task_id,
            record.runner_id().zip(record.runner_job_id()),
        )
        .map_err(|e| unavailable(e.to_string()))?;
        let root = plan
            .tasks
            .iter()
            .find(|task| task.task_id == outcome.task_id)
            .and_then(|task| task.workspace.root.as_deref())
            .ok_or_else(|| unavailable("missing destination workspace".into()))?;
        fetch_candidate_bundle(
            Path::new(root),
            Path::new(artifact.path.as_deref().unwrap()),
            &record.run_id,
            candidate,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) -> String {
        git(repo, args).unwrap_or_else(|error| panic!("git {args:?}: {error}"))
    }

    /// A runner clone holding a harvested candidate bundle, and a controller
    /// clone holding only the base.
    struct Fixture {
        _temp: tempfile::TempDir,
        runner: std::path::PathBuf,
        controller: std::path::PathBuf,
        bundle: std::path::PathBuf,
        candidate: AgentTaskCandidate,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let origin = temp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        run(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("file.txt"), "base\n").unwrap();
        run(&origin, &["add", "."]);
        run(
            &origin,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "base",
            ],
        );
        let [runner, controller] = ["runner", "controller"].map(|name| {
            let path = temp.path().join(name);
            run(
                temp.path(),
                &["clone", "-q", origin.to_str().unwrap(), name],
            );
            path
        });
        let base_sha = run(&runner, &["rev-parse", "HEAD"]);
        let candidate = commit_candidate(&runner, &base_sha, "agent change\n");
        let bundle = temp.path().join("candidate.bundle");
        bundle_candidate(&runner, &bundle, &base_sha);
        Fixture {
            _temp: temp,
            runner,
            controller,
            bundle,
            candidate,
        }
    }

    fn commit_candidate(repo: &Path, base: &str, contents: &str) -> AgentTaskCandidate {
        std::fs::write(repo.join("file.txt"), contents).unwrap();
        std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
        run(repo, &["add", "--all"]);
        let tree_sha = run(repo, &["write-tree"]);
        let identity = ["-c", "user.name=Homeboy", "-c", "user.email=h@localhost"];
        let commit = [
            &identity[..],
            &["commit-tree", &tree_sha, "-p", base, "-m", "c"],
        ]
        .concat();
        let candidate_sha = run(repo, &commit);
        run(
            repo,
            &[
                "update-ref",
                "refs/homeboy/candidates/run-1",
                &candidate_sha,
            ],
        );
        AgentTaskCandidate {
            base_sha: base.to_string(),
            candidate_sha,
            tree_sha,
        }
    }

    fn bundle_candidate(repo: &Path, bundle: &Path, base: &str) {
        let bundle = bundle.display().to_string();
        let exclude = format!("^{base}");
        run(
            repo,
            &[
                "bundle",
                "create",
                "-q",
                &bundle,
                "refs/homeboy/candidates/run-1",
                &exclude,
            ],
        );
    }

    fn assert_unpinned(controller: &Path) {
        assert!(run(controller, &["for-each-ref", "refs/homeboy/candidates"]).is_empty());
    }

    #[test]
    fn controller_fetches_the_identical_candidate_commit_and_tree() {
        let fixture = fixture();
        let candidate = &fixture.candidate;
        assert!(git(
            &fixture.controller,
            &["cat-file", "-e", &candidate.candidate_sha]
        )
        .is_err());

        let local_ref =
            fetch_candidate_bundle(&fixture.controller, &fixture.bundle, "run-1", candidate)
                .unwrap();

        assert_eq!(local_ref, "refs/homeboy/candidates/run-1");
        assert_eq!(
            run(&fixture.controller, &["rev-parse", &local_ref]),
            candidate.candidate_sha
        );
        let tree = run(
            &fixture.controller,
            &["rev-parse", &format!("{local_ref}^{{tree}}")],
        );
        assert_eq!(tree, candidate.tree_sha);
    }

    #[test]
    fn timeout_aggregate_fetches_before_controller_terminal_projection() {
        terminal_aggregate_fetch_fixture(false);
    }

    #[test]
    fn lab_terminal_harvest_promote_spans_are_read_back_from_store() {
        terminal_aggregate_fetch_fixture(true);
    }

    fn terminal_aggregate_fetch_fixture(promote: bool) {
        homeboy_core::test_support::with_isolated_home(|_| {
            use crate::agent_task_lifecycle as lifecycle;
            use crate::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
            let f = fixture();
            // Rename the ref to the actual controller-owned attempt run id.
            run(
                &f.runner,
                &[
                    "update-ref",
                    "refs/homeboy/candidates/timeout-run",
                    &f.candidate.candidate_sha,
                ],
            );
            run(
                &f.runner,
                &[
                    "bundle",
                    "create",
                    f.bundle.to_str().unwrap(),
                    "refs/homeboy/candidates/timeout-run",
                    &format!("^{}", f.candidate.base_sha),
                ],
            );
            let request = serde_json::from_value(serde_json::json!({"task_id":"task", "executor":{"backend":"fixture"}, "instructions":"edit", "workspace":{"root":f.controller}})).unwrap();
            let plan = AgentTaskPlan::new("timeout-plan", vec![request]);
            let mut record = lifecycle::submit_plan(&plan, Some("timeout-run")).unwrap();
            record
                .ensure_metadata_object()
                .insert("runner_id".into(), serde_json::json!("lab"));
            let bytes = std::fs::read(&f.bundle).unwrap();
            use sha2::Digest;
            let checksum = format!("{:x}", sha2::Sha256::digest(&bytes));
            let mut aggregate: AgentTaskAggregate = serde_json::from_value(serde_json::json!({
                "schema":"homeboy/agent-task-aggregate/v1", "plan_id":"timeout-plan", "status":"failed",
                "totals":{"timed_out":1,"skipped":0}, "outcomes":[{"task_id":"task", "status":"timeout", "candidate":f.candidate,
                "artifacts":[{"id":"bundle", "kind":"git_bundle", "path":f.bundle,"size_bytes":bytes.len(),"sha256":checksum}]}]
            })).unwrap();
            if promote {
                aggregate.outcomes[0].status = crate::agent_task::AgentTaskOutcomeStatus::Succeeded;
            }
            let store = lifecycle::AgentTaskLifecycleStore::from_current_environment().unwrap();
            let observation = store.open_observation_initialized().unwrap();
            observation
                .time_run_phase(&record.run_id, "stage.lab.observe", || Ok(()))
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
            // Use the authoritative transition, as detached terminal events do.
            super::super::super::agent_task_lifecycle::apply_aggregate_transition_in_store(
                &store,
                super::super::super::agent_task_lifecycle::AgentTaskAggregateTransition {
                    record: &mut record,
                    plan: &plan,
                    aggregate: &aggregate,
                },
            )
            .unwrap();
            assert_eq!(
                run(
                    &f.controller,
                    &["rev-parse", "refs/homeboy/candidates/timeout-run"]
                ),
                f.candidate.candidate_sha
            );
            assert_eq!(record.candidate, Some(f.candidate.clone()));
            if promote {
                let mut provider = super::super::tests::FakePromotionWorkspaceProvider::default();
                provider.workspace_path = Some(f.controller.clone());
                let request = super::super::AgentTaskPromotionRequest {
                    source: serde_json::to_string(&aggregate).unwrap(),
                    source_run_id: Some(record.run_id.clone()),
                    source_path: None,
                    source_worktree_path: Some(f.controller.clone()),
                    base_ref: None,
                    task_base_sha: Some(f.candidate.base_sha.clone()),
                    candidate_ref: None,
                    to_worktree: "controller@candidate".into(),
                    task_id: Some("task".into()),
                    artifact_id: None,
                    dry_run: false,
                    gates: crate::agent_task_gate::VerifyGateOptions::default(),
                    provider_invocation: None,
                    repository_integrity_evidence: None,
                };
                super::super::promote::promote_with_provider_and_checkpoint_in_observation_store(
                    request,
                    &mut provider,
                    &mut |_| Ok(()),
                    &observation,
                )
                .unwrap();
            }
            let spans = observation.phase_spans(&record.run_id).unwrap();
            if promote {
                assert!(spans.span("promote").unwrap().is_ok());
            }
            assert!(spans.span("promote.wait").unwrap().duration_ms.unwrap() >= 4.0);
            assert!(spans.span("promote.acquire").unwrap().is_ok());
        });
    }

    #[test]
    fn missing_bundle_is_non_recoverable_even_if_controller_already_has_commit() {
        let f = fixture();
        fetch_candidate_bundle(&f.controller, &f.bundle, "run-1", &f.candidate).unwrap();
        let request = serde_json::from_value(serde_json::json!({"task_id":"task", "executor":{"backend":"fixture"}, "instructions":"edit", "workspace":{"root":f.controller}})).unwrap();
        let plan = crate::agent_tasks::scheduler::AgentTaskPlan::new("plan", vec![request]);
        homeboy_core::test_support::with_isolated_home(|_| {
            let record = crate::agent_task_lifecycle::submit_plan(&plan, Some("run-1")).unwrap();
            let aggregate = serde_json::from_value(serde_json::json!({"plan_id":"plan", "status":"failed", "totals":{"skipped":0}, "outcomes":[{"task_id":"task", "status":"timeout", "candidate":f.candidate}]})).unwrap();
            let store =
                crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                    .unwrap();
            let error = fetch_run_candidates(&store, &record, &plan, &aggregate).unwrap_err();
            assert_eq!(error.retryable, Some(false));
            assert!(error.message.contains("non-recoverable"));
            assert_eq!(error.details["run_id"], "run-1");
            assert_eq!(error.details["expected"], f.candidate.candidate_sha);
        });
    }

    #[test]
    fn dirty_destination_is_refused_without_losing_user_bytes() {
        let f = fixture();
        fetch_candidate_bundle(&f.controller, &f.bundle, "run-1", &f.candidate).unwrap();
        std::fs::write(f.controller.join("user.txt"), "keep me").unwrap();
        let error = materialize_candidate_tree(&f.controller, &f.candidate).unwrap_err();
        assert!(error.message.contains("clean"));
        assert_eq!(
            std::fs::read_to_string(f.controller.join("user.txt")).unwrap(),
            "keep me"
        );
        assert_eq!(
            run(&f.controller, &["rev-parse", "HEAD"]),
            f.candidate.base_sha
        );
    }

    #[test]
    fn fetch_failure_never_resolves_an_unfetched_sha() {
        let f = fixture();
        let mut candidate = f.candidate.clone();
        candidate.candidate_sha = "unfetched-not-a-revision".into();
        std::fs::write(&f.bundle, "truncated").unwrap();
        let error = fetch_candidate_bundle(&f.controller, &f.bundle, "timeout-run", &candidate)
            .unwrap_err();
        assert!(error.message.contains("timeout-run"));
        assert!(error.message.contains("unfetched-not-a-revision"));
        assert!(!error.message.contains("single revision"));
        assert!(std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<CandidateBundleError>()
            .is_some());
    }

    #[test]
    fn tampered_bundle_fails_with_typed_commit_mismatch() {
        let fixture = fixture();
        // Replace the bundled commit under the same candidate ref.
        run(&fixture.runner, &["reset", "-q", "--hard"]);
        let base = &fixture.candidate.base_sha;
        let tampered = commit_candidate(&fixture.runner, base, "tampered\n");
        bundle_candidate(&fixture.runner, &fixture.bundle, base);

        let error = fetch_candidate_bundle(
            &fixture.controller,
            &fixture.bundle,
            "run-1",
            &fixture.candidate,
        )
        .unwrap_err();

        assert!(error.to_string().contains(&tampered.candidate_sha));
        let typed = error
            .source
            .as_ref()
            .unwrap()
            .downcast_ref::<CandidateBundleError>()
            .unwrap();
        assert_eq!(typed.kind, "commit_mismatch");
        assert_eq!(typed.expected, fixture.candidate.candidate_sha);
        assert_eq!(typed.observed, tampered.candidate_sha);
        assert_unpinned(&fixture.controller);
    }

    #[test]
    fn truncated_bundle_fails_with_typed_missing_object() {
        let fixture = fixture();
        let bytes = std::fs::read(&fixture.bundle).unwrap();
        std::fs::write(&fixture.bundle, &bytes[..bytes.len() / 2]).unwrap();

        let error = fetch_candidate_bundle(
            &fixture.controller,
            &fixture.bundle,
            "run-1",
            &fixture.candidate,
        )
        .unwrap_err();

        let typed = error
            .source
            .as_ref()
            .unwrap()
            .downcast_ref::<CandidateBundleError>()
            .unwrap();
        assert_eq!(typed.kind, "missing_object");
        assert_eq!(typed.expected, fixture.candidate.candidate_sha);
        assert!(
            error.message.contains(&fixture.candidate.candidate_sha),
            "{}",
            error.message
        );
        assert_unpinned(&fixture.controller);
    }

    #[test]
    fn recorded_tree_mismatch_fails_with_typed_tree_mismatch() {
        let fixture = fixture();
        let mut recorded = fixture.candidate.clone();
        recorded.tree_sha = run(&fixture.controller, &["rev-parse", "HEAD^{tree}"]);

        let error =
            fetch_candidate_bundle(&fixture.controller, &fixture.bundle, "run-1", &recorded)
                .unwrap_err();

        let typed = error
            .source
            .as_ref()
            .unwrap()
            .downcast_ref::<CandidateBundleError>()
            .unwrap();
        assert_eq!(typed.kind, "tree_mismatch");
        assert_eq!(typed.expected, recorded.tree_sha);
        assert_eq!(typed.observed, fixture.candidate.tree_sha);
        let message = error.message;
        assert!(
            message.contains(&recorded.tree_sha) && message.contains(&fixture.candidate.tree_sha)
        );
        assert_unpinned(&fixture.controller);
    }
}
