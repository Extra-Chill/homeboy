//! `agent-task history --task-url` reads attempt history from the durable run
//! store (#18451).

use super::support::*;
use crate::cli_surface::{Cli, Commands};
use clap::Parser;
use homeboy::agents::agent_task::AgentTaskCandidate;
use homeboy::agents::agent_task_service::COOK_DELIVERY_SCHEMA;

const ISSUE_URL: &str = "https://github.com/Extra-Chill/blocks-engine/issues/6000";
const PR_URL: &str = "https://github.com/Extra-Chill/blocks-engine/pull/6023";

fn persist_cook_run(
    run_id: &str,
    cook_id: &str,
    task_url: &str,
    admitted_at: &str,
    state: RunExecutionState,
    // Durable delivery/failure facts; the store derives the terminal outcome.
    facts: Value,
    candidate_sha: Option<&str>,
) {
    let mut plan = test_plan();
    plan.tasks[0].workspace.task_url = Some(task_url.to_string());
    agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist Cook run");
    agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
        record.submitted_at = admitted_at.to_string();
        record.updated_at = Some(admitted_at.to_string());
        record.set_state_for_test(state);
        record.lifecycle.execution.state = state.into();
        record.lifecycle.execution.finished_at = Some(admitted_at.replace("T00", "T01"));
        record.metadata["cook_id"] = json!(cook_id);
        for (key, value) in facts.as_object().into_iter().flatten() {
            record.metadata[key] = value.clone();
        }
        record.candidate = candidate_sha.map(|sha| AgentTaskCandidate {
            base_sha: "base".to_string(),
            candidate_sha: sha.to_string(),
            tree_sha: "tree".to_string(),
        });
    })
    .expect("settle Cook run");
}

fn history(args: &[&str]) -> Value {
    let cli = Cli::try_parse_from(
        ["homeboy", "agent-task", "history"]
            .into_iter()
            .chain(args.iter().copied()),
    )
    .expect("history parses");
    let Commands::AgentTask(args) = cli.command else {
        panic!("expected agent-task");
    };
    let (value, exit_code) = crate::commands::agent_task::run(args).expect("history succeeds");
    assert_eq!(exit_code, 0);
    value
}

#[test]
fn history_returns_issue_cook_and_pr_task_rework_newest_first_with_outcomes() {
    with_isolated_home(|_| {
        // Cook 1 worked the issue and published the PR.
        persist_cook_run(
            "cook-issue",
            "cook-issue",
            ISSUE_URL,
            "2026-11-18T00:00:00Z",
            RunExecutionState::Succeeded,
            json!({"cook_delivery": {"schema": COOK_DELIVERY_SCHEMA, "delivery": {
                "state": "published",
                "publication": {"status": "review_ready", "published": true, "draft": false, "pr_url": PR_URL},
            }}}),
            Some("aaa111"),
        );
        // Cook 2 is a PR task: a rework of that PR whose gate refused.
        persist_cook_run(
            "cook-rework",
            "cook-rework",
            PR_URL,
            "2026-11-18T02:00:00Z",
            RunExecutionState::Failed,
            json!({"cook_delivery": {"schema": COOK_DELIVERY_SCHEMA, "delivery": {
                "state": "blocked",
                "gates": [{"id": "gate-1", "state": "failed"}],
                "blocker": {"code": "gate_failed", "message": "cargo test failed", "evidence": "/cook_delivery/blocker"},
            }}}),
            Some("bbb222"),
        );
        persist_cook_run(
            "cook-rework-attempt-1",
            "cook-rework",
            PR_URL,
            "2026-11-18T03:00:00Z",
            RunExecutionState::Failed,
            json!({"pre_execution_failure": {"error_code": "runner.lost", "message": "runner lost its workspace"}}),
            None,
        );
        persist_cook_run(
            "cook-unrelated",
            "cook-unrelated",
            "https://github.com/Extra-Chill/blocks-engine/issues/1",
            "2026-11-18T04:00:00Z",
            RunExecutionState::Succeeded,
            json!({}),
            None,
        );

        let page = history(&["--task-url", PR_URL]);
        assert_eq!(page["schema"], "homeboy/agent-task-history/v1");
        assert_eq!(page["search_status"], "complete");
        let entries = page["entries"].as_array().expect("entries");
        let row = |field: &str| {
            entries
                .iter()
                .map(|entry| entry[field].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            row("run_id"),
            ["cook-rework-attempt-1", "cook-rework", "cook-issue"]
        );
        assert_eq!(row("cook_id"), ["cook-rework", "cook-rework", "cook-issue"]);
        assert_eq!(row("kind"), ["attempt", "cook", "cook"]);
        assert_eq!(
            row("outcome"),
            ["infrastructure", "gate_refused", "published"]
        );
        assert_eq!(row("failure_class"), ["runner.lost", "gate_refused", ""]);
        assert_eq!(
            row("summary"),
            [
                "runner lost its workspace",
                "gate-1 refused: cargo test failed",
                &format!("published: {PR_URL}"),
            ]
        );
        assert_eq!(row("candidate_head_sha"), ["", "bbb222", "aaa111"]);
        assert_eq!(row("task_url"), [PR_URL, PR_URL, ISSUE_URL]);
        assert_eq!(entries[2]["pr_url"], PR_URL);
        assert_eq!(entries[2]["admitted_at"], "2026-11-18T00:00:00Z");
        assert_eq!(entries[2]["finished_at"], "2026-11-18T01:00:00Z");

        // Bounded with a cursor: one entry per page walks the same order.
        let first = history(&["--task-url", PR_URL, "--limit", "1"]);
        assert_eq!(first["entries"][0]["run_id"], "cook-rework-attempt-1");
        let cursor = first["next_cursor"].as_str().expect("continuation");
        let second = history(&["--task-url", PR_URL, "--limit", "1", "--cursor", cursor]);
        assert_eq!(second["entries"][0]["run_id"], "cook-rework");

        // The same page is served over REST.
        crate::commands::agent_task::history::register_task_history_resource();
        let response = homeboy::core::http_api::handle(homeboy::core::http_api::HttpApiRequest {
            method: homeboy::core::http_api::HttpMethod::Get,
            path: format!("/v1/control-plane/task-history?task_url={PR_URL}"),
            body: None,
        })
        .expect("task-history HTTP route");
        assert_eq!(response.status, 200);
        assert_eq!(response.endpoint, "control_plane.task_history");
        let body = serde_json::to_value(&response.body).unwrap();
        assert_eq!(body["resource"]["entries"], page["entries"]);
    });
}
