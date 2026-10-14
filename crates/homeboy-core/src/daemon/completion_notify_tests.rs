//! Completion-notification shaping for one daemon pass (#15853).

use super::completion_notifications;
use crate::notification_payload::NotifyEventKind;
use crate::observation::RunRecord;

fn run(id: &str, status: &str, finished_at: &str, owner: Option<&str>) -> RunRecord {
    let mut metadata = serde_json::json!({});
    if let Some(owner) = owner {
        metadata["parent_run_id"] = serde_json::json!(owner);
    }
    RunRecord {
        id: id.to_string(),
        kind: "agent-task".to_string(),
        component_id: Some("blocks-engine".to_string()),
        started_at: "2026-10-13T15:00:00Z".to_string(),
        finished_at: Some(finished_at.to_string()),
        status: status.to_string(),
        command: Some(format!(
            "homeboy agent-task gate-execute --run {id} --very-long-argv"
        )),
        cwd: None,
        homeboy_version: None,
        git_sha: None,
        rig_id: None,
        metadata_json: metadata,
    }
}

fn claimed(runs: Vec<RunRecord>) -> Vec<(String, Option<RunRecord>)> {
    runs.into_iter()
        .map(|run| (run.id.clone(), Some(run)))
        .collect()
}

#[test]
fn a_stale_sweep_is_one_summary_not_one_notification_per_run() {
    let mut runs: Vec<RunRecord> = (0..25)
        .map(|index| {
            run(
                &format!("wave-gate-{index:02}"),
                "stale",
                &format!("2026-10-13T15:32:28.{index:02}Z"),
                Some(if index % 2 == 0 { "cook-a" } else { "cook-b" }),
            )
        })
        .collect();
    runs.push(run("live-cook", "pass", "2026-10-13T15:32:29Z", None));

    let notifications = completion_notifications(claimed(runs));

    assert_eq!(
        notifications.len(),
        2,
        "one pass result + one stale summary"
    );
    let (pass, pass_marker) = &notifications[0];
    assert_eq!(pass.run_id, "live-cook");
    assert!(pass_marker.is_some(), "per-run events keep their marker");

    let (summary, summary_marker) = &notifications[1];
    assert!(
        summary_marker.is_none(),
        "member markers are already claimed"
    );
    assert_eq!(summary.kind, NotifyEventKind::NeedsAttention);
    assert_eq!(summary.title, "homeboy 25 runs stale");
    let body = &summary.body;
    assert!(body.contains("Runs: 25"), "{body}");
    assert!(
        body.contains("Oldest finished: 2026-10-13T15:32:28.00Z"),
        "{body}"
    );
    assert!(
        body.contains("Newest finished: 2026-10-13T15:32:28.24Z"),
        "{body}"
    );
    assert!(body.contains("Owners: cook-a, cook-b"), "{body}");
    assert!(body.contains("wave-gate-00"), "{body}");
    assert!(body.contains("and 15 more"), "{body}");
    assert!(
        !body.contains("--very-long-argv"),
        "no per-run command lines: {body}"
    );
    let payload = summary.payload.as_ref().expect("payload");
    let commands: Vec<&str> = payload.actions.iter().map(|a| a.command.as_str()).collect();
    assert_eq!(
        commands,
        ["homeboy runs list --status stale --since 2026-10-13T15:00:00Z"]
    );
}

#[test]
fn a_lone_stale_run_keeps_its_own_notification() {
    let notifications = completion_notifications(claimed(vec![
        run("only-stale", "stale", "2026-10-13T15:32:28Z", None),
        run("failed", "fail", "2026-10-13T15:32:28Z", None),
    ]));

    let ids: Vec<&str> = notifications
        .iter()
        .map(|(event, _)| event.run_id.as_str())
        .collect();
    assert_eq!(ids, ["only-stale", "failed"]);
    assert!(notifications.iter().all(|(_, marker)| marker.is_some()));
    assert!(notifications[0].0.body.contains("--very-long-argv"));
}
