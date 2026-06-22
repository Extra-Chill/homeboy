//! Agent-task command promotion source resolution and review/loop reporting tests.

use super::support::*;

#[test]
fn promotion_source_resolves_completed_run_id() {
    with_temp_home(|| {
        let run_id = "run-promotion-source";

        run_loaded_plan(test_plan(), Some(run_id), InspectingExecutor::noop(run_id))
            .expect("run completed");

        let (raw, path) = review::read_promotion_source(run_id).expect("promotion source resolved");

        assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
        assert_eq!(
            path.as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str()),
            Some("aggregate.json")
        );
    });
}

#[test]
fn promotion_source_reads_bare_json_file_path() {
    let file = tempfile::NamedTempFile::new().expect("source file");
    std::fs::write(
        file.path(),
        r#"{"schema":"homeboy/agent-task-aggregate/v1"}"#,
    )
    .expect("write source");

    let (raw, path) = review::read_promotion_source(&file.path().display().to_string())
        .expect("promotion source file resolved");

    assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
    assert_eq!(path.as_deref(), Some(file.path()));
}

#[test]
fn review_reports_queued_run_without_chat_state() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-review-queued"))
            .expect("submitted");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-queued".to_string(),
            to_worktree: None,
            provider_command: None,
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-review/v1");
        assert_eq!(value["run_id"], "run-review-queued");
        assert_eq!(value["state"], "queued");
        assert_eq!(value["transport"]["chat_state_required"], false);
        assert!(value["aggregate_review"].is_null());
        assert_eq!(value["logs"]["events"][0]["state"], "queued");
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("run-next"));
    });
}

#[test]
fn review_reports_completed_aggregate_and_promotion_hints() {
    with_temp_home(|| {
        run_loaded_plan(
            test_plan(),
            Some("run-review-completed"),
            ApplyArtifactExecutor,
        )
        .expect("run completed");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-completed".to_string(),
            to_worktree: Some("homeboy@fix-review-flow".to_string()),
            provider_command: None,
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["state"], "succeeded");
        assert_eq!(value["aggregate_review"]["summary"]["apply_candidates"], 1);
        assert_eq!(value["artifacts"]["artifacts"][0]["id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["task_id"], "task-a");
        assert_eq!(value["promotion_candidates"][0]["artifact_id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["ready"], true);
        assert_eq!(
            value["promotion_candidates"][0]["command"],
            json!([
                "homeboy",
                "agent-task",
                "promote",
                value["aggregate_path"].as_str().expect("aggregate path"),
                "--task-id",
                "task-a",
                "--artifact-id",
                "patch-a",
                "--to-worktree",
                "homeboy@fix-review-flow"
            ])
        );
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("promotion_candidates"));
    });
}
