use std::path::Path;
use std::time::{Duration, Instant};

use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{
    merge_metadata, ActiveObservation, NewRunRecord, ObservationStore, RunStatus,
};
use homeboy::core::ObservationOutputMetadata;
use homeboy_review::review::{
    artifact_command, ReviewArtifactFindings, ReviewCommandOutput, ReviewStage,
};
use serde::Serialize;

use super::ReviewArgs;

pub(super) struct ReviewObservation(ActiveObservation);

impl ReviewObservation {
    pub(super) fn output_metadata(&self) -> ObservationOutputMetadata {
        ObservationOutputMetadata::for_run(&self.0.run().kind, self.0.run_id())
    }

    pub(super) fn run_id(&self) -> &str {
        self.0.run_id()
    }

    pub(super) fn progress(&self, phase: &str, current: &str, status: &str) {
        let Ok(Some(run)) = self.0.store().get_run(self.run_id()) else {
            return;
        };
        let metadata = merge_metadata(
            run.metadata_json,
            serde_json::json!({
                "observation_status": "running",
                "progress": {
                    "phase": phase,
                    "current": current,
                    "status": status,
                    "updated_at": chrono::Utc::now().to_rfc3339(),
                },
            }),
        );
        let _ = self
            .0
            .store()
            .update_running_run_metadata(self.run_id(), metadata);
        // The stream mirrors the persisted update so compact output remains
        // observable without making stderr the source of truth.
        eprintln!(
            "{}",
            serde_json::json!({
                "schema": "homeboy/review-lifecycle/v1",
                "event": "progress",
                "run_id": self.run_id(),
                "phase": phase,
                "current": current,
                "status": status,
            })
        );
    }

    pub(super) fn transfer_owner_to(&self, owner_pid: u32) -> homeboy::core::Result<()> {
        if self.0.transfer_owner_to(owner_pid)? {
            return Ok(());
        }
        Err(homeboy::core::Error::validation_invalid_argument(
            "run_id",
            "running observation ended before detached ownership transfer",
            Some(self.run_id().to_string()),
            None,
        ))
    }
}

#[derive(Serialize)]
struct EarlyReviewLifecycle<'a> {
    schema: &'static str,
    event: &'static str,
    run_id: &'a str,
    status: &'static str,
    show_command: String,
    watch_command: String,
    cancel_command: String,
}

pub(super) struct ReviewObservationStart<'a> {
    pub component_id: Option<&'a str>,
    pub component_label: Option<&'a str>,
    pub source_path: Option<&'a Path>,
    pub args: &'a ReviewArgs,
    pub scope: &'a str,
    pub changed_file_count: Option<usize>,
}

pub(super) fn start(start: ReviewObservationStart<'_>) -> homeboy::core::Result<ReviewObservation> {
    let metadata = review_observation_initial_metadata(
        start.component_label.unwrap_or("pending-discovery"),
        start.args,
        start.scope,
        start.changed_file_count,
    );
    let record = NewRunRecord::builder("review")
        .optional_cwd_path(start.source_path)
        .command(review_observation_command(
            start.component_id.unwrap_or("pending-discovery"),
            start.args,
        ))
        .current_homeboy_version()
        .git_sha(start.source_path.and_then(short_head_revision_at))
        .metadata(metadata.clone())
        .build();
    let record = match start.component_id {
        Some(component_id) => NewRunRecord {
            component_id: Some(component_id.to_string()),
            ..record
        },
        None => record,
    };
    ActiveObservation::start(record).map(ReviewObservation)
}

pub(super) fn resume(run_id: &str) -> homeboy::core::Result<ReviewObservation> {
    ActiveObservation::resume(run_id).map(ReviewObservation)
}

/// A child must not start writing lifecycle state until the launcher has made
/// its PID durable. This closes the launcher's exit-to-first-progress window.
pub(super) fn resume_after_ownership_transfer(
    run_id: &str,
    deadline: Duration,
) -> homeboy::core::Result<ReviewObservation> {
    let started = Instant::now();
    loop {
        let store = ObservationStore::open_initialized_for_lifecycle()?;
        let run = store.get_run(run_id)?.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "run_id",
                "run record not found",
                Some(run_id.to_string()),
                None,
            )
        })?;
        if run.status != RunStatus::Running.as_str() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "run_id",
                "detached observation ended before ownership transfer",
                Some(run_id.to_string()),
                None,
            ));
        }
        if homeboy::core::observation::run_owner_pid(&run) == Some(std::process::id()) {
            return ActiveObservation::resume(run_id).map(ReviewObservation);
        }
        if started.elapsed() >= deadline {
            return Err(homeboy::core::Error::internal_unexpected(format!(
                "timed out waiting {deadline:?} for detached review ownership transfer"
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Emit a JSONL lifecycle event after persistence. The final stdout envelope is unchanged.
pub(super) fn emit_early_lifecycle(observation: &Option<ReviewObservation>) {
    let Some(observation) = observation else {
        return;
    };
    let run_id = observation.0.run_id();
    let lifecycle = EarlyReviewLifecycle {
        schema: "homeboy/review-lifecycle/v1",
        event: "persisted",
        run_id,
        status: "running",
        show_command: format!("homeboy runs show {run_id}"),
        watch_command: format!("homeboy runs watch {run_id}"),
        cancel_command: format!("homeboy runs cancel {run_id}"),
    };
    if let Ok(json) = serde_json::to_string(&lifecycle) {
        eprintln!("{json}");
    }
}

pub(super) fn is_cancelled(run_id: &str) -> bool {
    ObservationStore::open_initialized()
        .ok()
        .and_then(|store| store.get_run(run_id).ok().flatten())
        .is_some_and(|run| run.status == RunStatus::Skipped.as_str())
}

pub(super) fn finish_success(
    observation: Option<ReviewObservation>,
    output: &ReviewCommandOutput,
    exit_code: i32,
) {
    let Some(observation) = observation else {
        return;
    };

    let status = if !output.audit.ran && !output.lint.ran && !output.test.ran {
        RunStatus::Skipped
    } else if output.summary.passed {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    let metadata = review_observation_finish_metadata(
        observation.0.initial_metadata().clone(),
        output,
        exit_code,
        None,
    );
    finish_if_running(&observation.0, status, Some(metadata));
}

pub(super) fn finish_error(observation: Option<ReviewObservation>, error: &homeboy::core::Error) {
    let Some(observation) = observation else {
        return;
    };

    let metadata = merge_metadata(
        observation.0.initial_metadata().clone(),
        serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
        }),
    );
    finish_if_running(&observation.0, RunStatus::Error, Some(metadata));
}

/// Persist direct child output on the umbrella review run. Direct `review test`
/// has no `ReviewCommandOutput`, but its evidence must remain discoverable from
/// the run that the detached launcher announced.
pub(super) fn finish_direct_success(
    observation: Option<ReviewObservation>,
    stage: &str,
    output: serde_json::Value,
    exit_code: i32,
) {
    let Some(observation) = observation else {
        return;
    };
    let status = if exit_code == 0 {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    let metadata = merge_metadata(
        observation.0.initial_metadata().clone(),
        serde_json::json!({
            "observation_status": status.as_str(),
            "exit_code": exit_code,
            "stages": [{
                "name": stage,
                "status": status.as_str(),
                "ran": true,
                "exit_code": exit_code,
                "output": output,
            }],
        }),
    );
    finish_if_running(&observation.0, status, Some(metadata));
}

fn finish_if_running(
    observation: &ActiveObservation,
    status: RunStatus,
    metadata: Option<serde_json::Value>,
) {
    let Some(metadata) = metadata else {
        let _ = observation
            .store()
            .finish_running_run(observation.run_id(), status, None);
        return;
    };
    // A heartbeat can race the terminal path. CAS against the metadata we read
    // so the terminal record retains the newest progress instead of restoring
    // the launcher's initial snapshot over it.
    for _ in 0..3 {
        let Ok(Some(current)) = observation.store().get_run(observation.run_id()) else {
            return;
        };
        if current.status != RunStatus::Running.as_str() {
            return;
        }
        let merged = merge_metadata(current.metadata_json.clone(), metadata.clone());
        match observation.store().finish_running_run_if_metadata(
            observation.run_id(),
            status,
            merged,
            &current.metadata_json,
        ) {
            Ok(Some(_)) => return,
            Ok(None) => continue,
            Err(_) => return,
        }
    }
}

fn review_observation_command(component_id: &str, args: &ReviewArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "review".to_string(),
        component_id.to_string(),
    ];
    if let Some(changed_since) = args.changed.changed_since() {
        parts.push(format!("--changed-since={changed_since}"));
    }
    if args.changed.changed_only {
        parts.push("--changed-only".to_string());
    }
    if args.summary {
        parts.push("--summary".to_string());
    }
    if let Some(report) = args.report.as_ref() {
        parts.push(format!("--report={report}"));
    }
    parts.join(" ")
}

pub(super) fn review_observation_initial_metadata(
    component_label: &str,
    args: &ReviewArgs,
    scope: &str,
    changed_file_count: Option<usize>,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/review-observation/v1",
        "component_label": component_label,
        "scope": scope,
        "changed_since": args.changed.changed_since(),
        "changed_only": args.changed.changed_only,
        "summary": args.summary,
        "ci_profile": args.ci_profile,
        "report": args.report,
        "changed_file_count": changed_file_count,
        "execution_provenance": crate::commands::utils::execution_provenance::captured(),
        "observation_status": "running",
    })
}

pub(super) fn review_observation_finish_metadata(
    initial_metadata: serde_json::Value,
    output: &ReviewCommandOutput,
    exit_code: i32,
    error: Option<&str>,
) -> serde_json::Value {
    let mut stages = vec![
        stage_observation(&output.audit),
        stage_observation(&output.lint),
        stage_observation(&output.test),
    ];
    if let Some(ref stage) = output.ci_profile {
        stages.push(stage_observation(stage));
    }

    merge_metadata(
        initial_metadata,
        serde_json::json!({
            "observation_status": output.artifact.status,
            "exit_code": exit_code,
            "passed": output.summary.passed,
            "status": output.summary.status,
            "total_findings": output.summary.total_findings,
            "changed_file_count": output.summary.changed_file_count,
            "hints": output.summary.hints,
            "artifact": output.artifact,
            "stages": stages,
            "error": error,
        }),
    )
}

fn stage_observation<T: serde::Serialize + ReviewArtifactFindings>(
    stage: &ReviewStage<T>,
) -> serde_json::Value {
    let command = artifact_command(stage);
    serde_json::json!({
        "name": command.name,
        "status": command.status,
        "ran": stage.ran,
        "passed": stage.passed,
        "exit_code": command.exit_code,
        "finding_count": stage.finding_count,
        "summary": command.summary,
        "hint": stage.hint,
        "skipped_reason": stage.skipped_reason,
        "run_id": stage.output.as_ref().and_then(stage_run_id),
    })
}

fn stage_run_id<T: serde::Serialize>(output: &T) -> Option<String> {
    serde_json::to_value(output)
        .ok()?
        .pointer("/_homeboy_actionable/run/id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{
        BaselineArgs, ChangedScopeArgs, ChangedSinceArgs, ExtensionOverrideArgs,
        LabChangedScopeArgs, PositionalComponentArgs,
    };
    use homeboy_review::review::{build_artifact, ReviewSummary};

    fn review_args() -> ReviewArgs {
        ReviewArgs {
            command: None,
            run_id: None,
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            changed: ChangedScopeArgs {
                lab: LabChangedScopeArgs {
                    since: ChangedSinceArgs {
                        changed_since: Some("origin/main".to_string()),
                        precomputed_changed_files: None,
                    },
                    lab_changed_files_json: None,
                },
                changed_only: false,
            },
            summary: true,
            ci_profile: None,
            audit_profile: None,
            report: Some("pr-comment".to_string()),
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        }
    }

    #[test]
    fn initial_metadata_captures_review_scope() {
        let args = review_args();
        let metadata =
            review_observation_initial_metadata("homeboy", &args, "changed-since", Some(3));

        assert_eq!(metadata["schema"], "homeboy/review-observation/v1");
        assert_eq!(metadata["component_label"], "homeboy");
        assert_eq!(metadata["scope"], "changed-since");
        assert_eq!(metadata["changed_since"], "origin/main");
        assert_eq!(metadata["changed_file_count"], 3);
        assert_eq!(metadata["observation_status"], "running");
    }

    #[test]
    fn persisted_lifecycle_survives_an_interrupted_foreground_client() {
        homeboy::test_support::with_isolated_home(|_| {
            let args = review_args();
            let observation = start(ReviewObservationStart {
                component_id: Some("homeboy"),
                component_label: Some("homeboy"),
                source_path: Some(Path::new("/tmp/homeboy")),
                args: &args,
                scope: "changed-since",
                changed_file_count: Some(3),
            })
            .expect("persisted observation");
            let run_id = observation.run_id().to_string();

            // No terminal cleanup runs after this point, as if the caller timed out.
            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read")
                .expect("run");
            assert_eq!(run.kind, "review");
            assert_eq!(run.status, "running");

            observation.progress("dependency_setup", "fixture-provider", "heartbeat");
            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read")
                .expect("run");
            assert_eq!(run.metadata_json["progress"]["phase"], "dependency_setup");
            assert_eq!(run.metadata_json["progress"]["current"], "fixture-provider");

            let lifecycle = serde_json::to_value(EarlyReviewLifecycle {
                schema: "homeboy/review-lifecycle/v1",
                event: "persisted",
                run_id: &run_id,
                status: "running",
                show_command: format!("homeboy runs show {run_id}"),
                watch_command: format!("homeboy runs watch {run_id}"),
                cancel_command: format!("homeboy runs cancel {run_id}"),
            })
            .expect("lifecycle JSON");
            assert_eq!(lifecycle["run_id"], run_id);
            assert_eq!(
                lifecycle["show_command"],
                format!("homeboy runs show {run_id}")
            );
            assert_eq!(
                lifecycle["watch_command"],
                format!("homeboy runs watch {run_id}")
            );
            assert_eq!(
                lifecycle["cancel_command"],
                format!("homeboy runs cancel {run_id}")
            );

            let (attached, exit_code) = super::super::attach_to_persisted_review(&run_id)
                .expect("attach to existing review");
            assert_eq!(exit_code, 0);
            assert_eq!(attached.observation.expect("observation").run_id, run_id);
            let review_runs = ObservationStore::open_initialized()
                .expect("store")
                .list_runs(homeboy::core::observation::RunListFilter {
                    kind: Some("review".to_string()),
                    ..Default::default()
                })
                .expect("review runs");
            assert_eq!(
                review_runs.len(),
                1,
                "attaching must not create duplicate work"
            );
        });
    }

    #[test]
    fn detached_owner_transfer_survives_reconcile_and_preserves_progress_to_terminal_result() {
        homeboy::test_support::with_isolated_home(|_| {
            let args = review_args();
            let observation = start(ReviewObservationStart {
                component_id: Some("homeboy"),
                component_label: Some("homeboy"),
                source_path: Some(Path::new("/tmp/homeboy")),
                args: &args,
                scope: "changed-only",
                changed_file_count: Some(1),
            })
            .expect("launcher admission");
            let run_id = observation.run_id().to_string();

            // Model an exited launcher before it atomically hands the durable
            // row to its detached child (this test process).
            observation
                .transfer_owner_to(u32::MAX)
                .expect("launcher ownership");
            observation
                .transfer_owner_to(std::process::id())
                .expect("child ownership transfer");
            let store = ObservationStore::open_initialized().expect("store");
            let owned = store.get_run(&run_id).expect("read").expect("admitted run");
            assert_eq!(
                homeboy::core::observation::run_owner_pid(&owned),
                Some(std::process::id())
            );
            let watched = crate::commands::runs::poll_once_for_test(&store, &run_id)
                .expect("watch/reconcile live child");
            assert_eq!(watched.status, RunStatus::Running.as_str());

            observation.progress("test_execution", "review.test", "heartbeat");
            finish_direct_success(
                Some(observation),
                "test",
                serde_json::json!({ "result": "pass" }),
                0,
            );
            let terminal = store
                .get_run(&run_id)
                .expect("read terminal")
                .expect("terminal run");
            assert_eq!(terminal.status, RunStatus::Pass.as_str());
            assert_eq!(
                terminal.metadata_json["progress"]["phase"],
                "test_execution"
            );
            assert_eq!(terminal.metadata_json["stages"][0]["name"], "test");
        });
    }

    #[test]
    fn finish_metadata_captures_aggregate_and_linkable_stages() {
        use homeboy::core::code_audit::AuditCommandOutput;
        use homeboy_core::extension::lint::LintCommandOutput;
        use homeboy_core::extension::test::TestCommandOutput;

        let initial = serde_json::json!({
            "schema": "homeboy/review-observation/v1",
            "component_label": "homeboy",
            "scope": "changed-since",
            "observation_status": "running",
        });
        let audit = ReviewStage {
            stage: "audit".to_string(),
            ran: true,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: "Deep dive: homeboy review audit homeboy --changed-since=origin/main".to_string(),
            skipped_reason: None,
            output: None::<AuditCommandOutput>,
        };
        let lint = ReviewStage {
            stage: "lint".to_string(),
            ran: true,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: "Deep dive: homeboy review lint homeboy --changed-since=origin/main".to_string(),
            skipped_reason: None,
            output: None::<LintCommandOutput>,
        };
        let test = ReviewStage {
            stage: "test".to_string(),
            ran: true,
            passed: false,
            exit_code: 1,
            finding_count: 0,
            hint: "Run individually: homeboy review test".to_string(),
            skipped_reason: None,
            output: None::<TestCommandOutput>,
        };
        let artifact = build_artifact(
            "homeboy",
            "origin/main",
            "abc123",
            vec![
                artifact_command(&audit),
                artifact_command(&lint),
                artifact_command(&test),
            ],
        );
        let output = ReviewCommandOutput {
            command: "review".to_string(),
            plan: homeboy::core::quality::build_quality_plan(
                homeboy::core::quality::QualityPlanOptions::review("homeboy"),
            ),
            observation: None,
            artifact,
            summary: ReviewSummary {
                passed: false,
                status: "failed".to_string(),
                component: "homeboy".to_string(),
                scope: "changed-since".to_string(),
                changed_since: Some("origin/main".to_string()),
                total_findings: 0,
                changed_file_count: Some(3),
                hints: vec!["hint".to_string()],
            },
            audit,
            lint,
            test,
            ci_profile: None,
            actionable: None,
        };

        let metadata = review_observation_finish_metadata(initial, &output, 1, None);

        assert_eq!(metadata["observation_status"], "failed");
        assert_eq!(metadata["exit_code"], 1);
        assert_eq!(metadata["total_findings"], 0);
        assert_eq!(metadata["artifact"]["schema"], "homeboy/review/v1");
        assert_eq!(metadata["stages"].as_array().expect("stages").len(), 3);
        assert_eq!(metadata["stages"][2]["name"], "test");
        assert_eq!(metadata["stages"][2]["status"], "failed");
        assert_eq!(metadata["stages"][2]["finding_count"], 0);
        assert!(metadata["stages"][2].get("run_id").is_some());
    }
}
