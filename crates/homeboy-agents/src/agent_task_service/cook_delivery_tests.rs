use serde_json::{json, Value};

use super::*;

/// A post-apply promotion report as `record_promotion_in_store` persists it.
fn promotion(status: &str, gates: Value) -> Value {
    json!({
        "schema": "homeboy/agent-task-promotion-report/v1",
        "status": status,
        "task_id": "task-1",
        "source": { "kind": "aggregate", "task_id": "task-1", "run_id": "run-1" },
        "to_worktree": "homeboy@cook",
        "target": { "worktree": "homeboy@cook", "path": "/work/homeboy", "branch": "cook/fix", "head": "abc123" },
        "candidate": { "base_sha": "base", "candidate_sha": "patch-1", "tree_sha": "patchsha" },
        "changed_files": ["src/lib.rs"],
        "verified_base": { "base": "origin/main", "sha": "base-sha" },
        "deterministic_gates": gates,
        "provenance": {
            "post_apply": true,
            "worktree_path": "/work/homeboy",
            "candidate": { "kind": "git", "fingerprint": { "sha256": "fingerprint-sha", "head": "abc123" } }
        }
    })
}

fn passed_gate() -> Value {
    json!([{
        "id": "cargo-test",
        "status": "succeeded",
        "command": ["cargo", "test"],
        "exit_code": 0,
        "capture": { "stdout": { "sha256": "out-sha" }, "stderr": { "sha256": "err-sha" } }
    }])
}

fn failed_gate() -> Value {
    json!([{
        "id": "cargo-test",
        "status": "failed",
        "command": ["cargo", "test"],
        "exit_code": 101,
        "failure_evidence": { "summary": "2 tests failed" }
    }])
}

fn published(status: &str) -> Value {
    json!({
        "status": status,
        "path": "/work/homeboy",
        "base": "main",
        "head": "cook/fix",
        "pr_action": "created",
        "pr_number": 42,
        "pr_url": "https://github.com/Extra-Chill/homeboy/pull/42",
        "manual_finalization": false,
        "publication_proof": {
            "binding": { "candidate_sha": "commit-sha", "remote_sha": "remote-sha" }
        }
    })
}

struct Case {
    name: &'static str,
    metadata: Value,
    state: CookDeliveryState,
    blocker: Option<CookDeliveryBlockerCode>,
}

#[test]
fn from_run_metadata_reads_every_persisted_delivery_shape() {
    use CookDeliveryBlockerCode as Code;
    use CookDeliveryState as State;
    let gate_failed = |status: &str| Case {
        name: "gate failed",
        metadata: json!({ "promotion": promotion(status, failed_gate()) }),
        state: State::Blocked,
        blocker: Some(Code::GateFailed),
    };
    let cases = [
        Case {
            name: "applied, verification pending",
            metadata: json!({ "promotion": promotion("verification_pending", json!([])) }),
            state: State::Applied,
            blocker: None,
        },
        Case {
            name: "applied and verified but unpublished",
            metadata: json!({ "promotion": promotion("applied", passed_gate()) }),
            state: State::Verified,
            blocker: None,
        },
        Case {
            name: "published draft",
            metadata: json!({
                "promotion": promotion("applied", passed_gate()),
                "finalization": published("draft_published"),
            }),
            state: State::Published,
            blocker: None,
        },
        gate_failed("gate_failed"),
        gate_failed("no_changes_gate_failed"),
        Case {
            name: "gates deferred",
            metadata: json!({ "promotion": promotion(
                "gate_deferred",
                json!([{ "id": "lab-test", "status": "deferred" }]),
            ) }),
            state: State::Blocked,
            blocker: Some(Code::GatesDeferred),
        },
        Case {
            name: "intentional no change",
            metadata: json!({
                "promotion": { "status": "verified_no_changes" },
                "finalization": {
                    "schema": "agent-task-cook-no-change-finalization/v1",
                    "status": "intentional_no_change_finalized",
                    "disposition": "no_change_finalized",
                    "publication": { "committed": false, "pushed": false, "published": false }
                },
            }),
            state: State::NoChange,
            blocker: None,
        },
        Case {
            name: "provider produced no changes",
            metadata: json!({ "promotion": { "status": "no_changes" } }),
            state: State::NoChange,
            blocker: None,
        },
        Case {
            name: "manual finalization published",
            metadata: json!({
                "promotion": promotion("applied", passed_gate()),
                "manual": { "intent": { "path": "/work/homeboy" } },
                "finalization": {
                    "status": "review_ready",
                    "manual_finalization": true,
                    "pr": { "url": "https://github.com/Extra-Chill/homeboy/pull/43" }
                },
            }),
            state: State::Published,
            blocker: None,
        },
        Case {
            name: "manual finalization refused",
            metadata: json!({
                "promotion": promotion("applied", passed_gate()),
                "manual": {
                    "intent": { "path": "/work/homeboy" },
                    "failure": {
                        "status": "failed",
                        "phase": "publication",
                        "error": { "code": "git.push_rejected", "message": "push was rejected by the remote" }
                    },
                    "retry": true,
                },
            }),
            state: State::Blocked,
            blocker: Some(Code::PublishRefused),
        },
        Case {
            name: "finalization failed",
            metadata: json!({
                "promotion": promotion("applied", passed_gate()),
                "finalization": {
                    "status": "failed",
                    "code": "finalization.pr_create_failed",
                    "message": "pull request creation was denied"
                },
            }),
            state: State::Blocked,
            blocker: Some(Code::PublishRefused),
        },
    ];
    for case in cases {
        let delivery =
            CookDelivery::from_run_metadata(&delivery_metadata(case.metadata)).expect(case.name);
        assert_eq!(delivery.state, case.state, "{}", case.name);
        assert_eq!(delivery.blocker_code(), case.blocker, "{}", case.name);
    }
}

#[test]
fn every_gate_failure_spelling_names_the_failing_gate() {
    for status in ["gate_failed", "no_changes_gate_failed"] {
        let delivery = CookDelivery::from_run_metadata(&delivery_metadata(
            json!({ "promotion": promotion(status, failed_gate()) }),
        ))
        .expect(status);
        let blocker = delivery.blocker.expect(status);
        assert_eq!(
            blocker.message, "deterministic gate `cargo-test` failed: 2 tests failed",
            "{status}"
        );
        assert_eq!(blocker.evidence, "/cook_delivery/artifacts/promotion");
        assert_eq!(delivery.gates[0].state, AgentTaskGateStatus::Failed);
    }
}

/// The pre-#16049 spelling stopped parsing when its serde alias was retired
/// (#16390); a delivery carrying it is not a delivery this binary understands.
#[test]
fn the_retired_no_op_gate_failed_spelling_is_no_longer_a_delivery() {
    let delivery = CookDelivery::from_run_metadata(&delivery_metadata(
        json!({ "promotion": promotion("no_op_gate_failed", failed_gate()) }),
    ));

    assert!(delivery.is_none());
}

#[test]
fn publication_refusals_keep_the_writer_code_and_message() {
    let delivery = CookDelivery::from_run_metadata(&delivery_metadata(json!({
        "promotion": promotion("applied", passed_gate()),
        "manual": { "failure": {
            "status": "failed",
            "phase": "publication",
            "error": { "code": "git.push_rejected", "message": "push was rejected by the remote" }
        } },
    })))
    .expect("delivery");
    let blocker = delivery.blocker.expect("blocker");
    assert_eq!(blocker.code, CookDeliveryBlockerCode::PublishRefused);
    assert_eq!(blocker.message, "push was rejected by the remote");
    assert_eq!(blocker.cause.as_deref(), Some("git.push_rejected"));
    assert_eq!(blocker.evidence, "/cook_delivery/artifacts/manual/failure");
}

#[test]
fn published_delivery_reads_candidate_destination_gates_and_publication() {
    let delivery = CookDelivery::from_run_metadata(&delivery_metadata(json!({
        "promotion": promotion("applied", passed_gate()),
        "finalization": published("draft_published"),
    })))
    .expect("delivery");
    let candidate = delivery.candidate.expect("candidate");
    assert_eq!(candidate.promotion, AgentTaskPromotionStatus::Applied);
    assert_eq!(candidate.candidate.as_ref().unwrap().tree_sha, "patchsha");
    assert_eq!(candidate.verified_base.as_deref(), Some("base-sha"));
    assert!(candidate.target_applied);
    let destination = delivery.destination.expect("destination");
    assert_eq!(destination.path.as_deref(), Some("/work/homeboy"));
    assert_eq!(destination.head.as_deref(), Some("abc123"));
    assert_eq!(delivery.gates[0].state, AgentTaskGateStatus::Succeeded);
    assert_eq!(
        delivery.gates[0].proof_refs,
        vec!["sha256:out-sha".to_string(), "sha256:err-sha".to_string()]
    );
    let publication = delivery.publication.expect("publication");
    assert!(publication.published && publication.draft);
    assert_eq!(publication.pr_number, Some(42));
    assert_eq!(publication.commit.as_deref(), Some("commit-sha"));
    assert_eq!(publication.pushed_sha.as_deref(), Some("remote-sha"));
}

#[test]
fn a_pr_identity_without_a_publish_status_is_not_published() {
    for status in ["completed", "failed", "pending"] {
        let publication = publication_receipt(&json!({ "status": status, "pr_number": 42 }));
        assert!(!publication.published, "{status}");
        assert_eq!(publication.pr_identity().as_deref(), Some("42"), "{status}");
    }
}

/// Run metadata holding a `cook_delivery` record of these artifacts.
fn delivery_metadata(artifacts: Value) -> Value {
    test_support::cook_delivery_metadata_for_test(artifacts)
}

#[test]
fn runs_without_a_delivery_record_have_no_delivery() {
    for metadata in [
        json!({}),
        // The retired per-stage keys are not delivery evidence.
        json!({
            "latest_promotion": promotion("applied", passed_gate()),
            "cook_finalization": published("review_ready"),
        }),
        // A record of another schema version is not one this binary reads.
        json!({ "cook_delivery": { "schema": "homeboy/cook-delivery/v1", "state": "published" } }),
        // Neither is a record without delivery evidence.
        delivery_metadata(json!({ "promotion": null, "finalization": null })),
        delivery_metadata(json!({ "promotion": { "status": "dry_run" } })),
    ] {
        assert_eq!(
            CookDelivery::from_run_metadata(&metadata),
            None,
            "{metadata}"
        );
    }
}

#[test]
fn read_model_accessors_answer_from_the_record() {
    let mut failed_receipt = published("failed");
    failed_receipt["pr_url"] = Value::Null;
    failed_receipt["pull_request_url"] = json!("https://example.invalid/pr/9");
    failed_receipt["pr_number"] = Value::Null;
    let cases = [
        (
            "verification pending",
            json!({ "promotion": promotion("verification_pending", json!([])) }),
            (true, false, false, None),
        ),
        (
            "published",
            json!({
                "promotion": promotion("applied", passed_gate()),
                "finalization": published("review_ready"),
                "force_with_lease": { "remote_sha": "lease-sha" },
            }),
            (
                false,
                true,
                true,
                Some("https://github.com/Extra-Chill/homeboy/pull/42"),
            ),
        ),
        (
            "refused receipt",
            json!({
                "promotion": promotion("applied", passed_gate()),
                "finalization": failed_receipt,
            }),
            (false, true, false, Some("https://example.invalid/pr/9")),
        ),
    ];
    for (name, artifacts, (awaits, receipt, published, pr_url)) in cases {
        let delivery =
            CookDelivery::from_run_metadata(&delivery_metadata(artifacts)).expect("delivery");
        assert_eq!(delivery.awaits_verification(), awaits, "{name}");
        assert_eq!(delivery.has_publication_receipt(), receipt, "{name}");
        assert_eq!(delivery.published(), published, "{name}");
        assert_eq!(delivery.pr_url(), pr_url, "{name}");
        assert_eq!(
            delivery.candidate.unwrap().candidate.unwrap().tree_sha,
            "patchsha",
            "{name}"
        );
    }
}

#[test]
fn a_report_without_a_typed_candidate_has_no_candidate() {
    let mut latest = promotion("applied", passed_gate());
    latest["candidate"] = Value::Null;
    let delivery = CookDelivery::from_run_metadata(&delivery_metadata(json!({
        "promotion": latest,
        "source_checkpoint": promotion("verification_pending", json!([])),
    })))
    .expect("delivery");
    let candidate = delivery.candidate.unwrap();
    assert!(candidate.candidate.is_none());
    assert!(!candidate.target_applied);
}

#[test]
fn every_tolerated_pr_identity_path_reads_through_the_publication_receipt() {
    for (receipt, number, url) in [
        (
            json!({ "status": "review_ready", "pr_number": 7 }),
            Some(7),
            None,
        ),
        (
            json!({ "status": "review_ready", "pr": { "number": 7 } }),
            Some(7),
            None,
        ),
        (
            json!({ "status": "review_ready", "pr_url": "https://x/pr/7" }),
            None,
            Some("https://x/pr/7"),
        ),
        (
            json!({ "status": "review_ready", "pull_request_url": "https://x/pr/7" }),
            None,
            Some("https://x/pr/7"),
        ),
        (
            json!({ "status": "review_ready", "pr": { "url": "https://x/pr/7" } }),
            None,
            Some("https://x/pr/7"),
        ),
        // A blank URL is not an identity, and never hides one at another path.
        (
            json!({ "status": "review_ready", "pr_url": " ", "pull_request_url": "https://x/pr/7" }),
            None,
            Some("https://x/pr/7"),
        ),
    ] {
        let publication = publication_receipt(&receipt);
        assert!(publication.published, "{receipt}");
        assert_eq!(publication.pr_number, number, "{receipt}");
        assert_eq!(publication.pr_url.as_deref(), url, "{receipt}");
        let delivery = CookDelivery::from_run_metadata(&delivery_metadata(
            json!({ "finalization": receipt.clone() }),
        ))
        .expect("delivery");
        assert!(delivery.published(), "{receipt}");
        assert_eq!(delivery.pr_url(), url, "{receipt}");
    }
}
