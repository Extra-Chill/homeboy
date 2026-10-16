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
        "patch_artifact": { "id": "patch-1", "kind": "patch", "path": "/artifacts/patch-1.diff", "sha256": "patchsha" },
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

fn moving_base(blocker: &str, base_movements: u32) -> Value {
    json!({
        "schema": "homeboy/agent-task-cook-moving-base-recovery/v1",
        "cook_id": "cook-1",
        "run_id": "run-1",
        "promotion": { "changed_files": ["src/lib.rs"] },
        "prior_verified_base": "base-sha",
        "passed_gates": [],
        "blocker": blocker,
        "continuation": "homeboy agent-task cook-continue run-1",
        "base_movements": base_movements
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
        metadata: json!({ "latest_promotion": promotion(status, failed_gate()) }),
        state: State::Blocked,
        blocker: Some(Code::GateFailed),
    };
    let cases = [
        Case {
            name: "applied, verification pending",
            metadata: json!({ "latest_promotion": promotion("verification_pending", json!([])) }),
            state: State::Applied,
            blocker: None,
        },
        Case {
            name: "applied and verified but unpublished",
            metadata: json!({ "latest_promotion": promotion("applied", passed_gate()) }),
            state: State::Verified,
            blocker: None,
        },
        Case {
            name: "published draft",
            metadata: json!({
                "latest_promotion": promotion("applied", passed_gate()),
                "cook_finalization": published("draft_published"),
            }),
            state: State::Published,
            blocker: None,
        },
        gate_failed("gate_failed"),
        gate_failed("no_op_gate_failed"),
        gate_failed("no_changes_gate_failed"),
        Case {
            name: "gates deferred",
            metadata: json!({ "latest_promotion": promotion(
                "gate_deferred",
                json!([{ "id": "lab-test", "status": "deferred" }]),
            ) }),
            state: State::Blocked,
            blocker: Some(Code::GatesDeferred),
        },
        Case {
            name: "moving base blocked (#15017)",
            metadata: json!({
                "latest_promotion": promotion("applied", passed_gate()),
                "cook_moving_base_recovery": moving_base("base moved during promotion", 1),
            }),
            state: State::Blocked,
            blocker: Some(Code::BaseConflict),
        },
        Case {
            name: "destination changed outside the controller",
            metadata: json!({
                "latest_promotion": promotion("applied", passed_gate()),
                "cook_moving_base_recovery": moving_base(
                    "remote branch differs from the exact promoted candidate",
                    3,
                ),
            }),
            state: State::Blocked,
            blocker: Some(Code::ForeignCommits),
        },
        Case {
            name: "intentional no change",
            metadata: json!({
                "latest_promotion": { "status": "verified_no_changes" },
                "cook_finalization": {
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
            metadata: json!({ "latest_promotion": { "status": "no_changes" } }),
            state: State::NoChange,
            blocker: None,
        },
        Case {
            name: "manual finalization published",
            metadata: json!({
                "latest_promotion": promotion("applied", passed_gate()),
                "manual_finalization_intent": { "path": "/work/homeboy" },
                "cook_finalization": {
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
                "latest_promotion": promotion("applied", passed_gate()),
                "manual_finalization_intent": { "path": "/work/homeboy" },
                "manual_finalization_failure": {
                    "status": "failed",
                    "phase": "publication",
                    "error": { "code": "git.push_rejected", "message": "push was rejected by the remote" }
                },
                "manual_finalization_retry": true,
            }),
            state: State::Blocked,
            blocker: Some(Code::PublishRefused),
        },
        Case {
            name: "finalization failed",
            metadata: json!({
                "latest_promotion": promotion("applied", passed_gate()),
                "cook_finalization": {
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
        let delivery = CookDelivery::from_run_metadata(&case.metadata).expect(case.name);
        assert_eq!(delivery.state, case.state, "{}", case.name);
        assert_eq!(delivery.blocker_code(), case.blocker, "{}", case.name);
    }
}

#[test]
fn every_gate_failure_spelling_names_the_failing_gate() {
    for status in ["gate_failed", "no_op_gate_failed", "no_changes_gate_failed"] {
        let delivery = CookDelivery::from_run_metadata(
            &json!({ "latest_promotion": promotion(status, failed_gate()) }),
        )
        .expect(status);
        let blocker = delivery.blocker.expect(status);
        assert_eq!(
            blocker.message, "deterministic gate `cargo-test` failed: 2 tests failed",
            "{status}"
        );
        assert_eq!(blocker.evidence, "/latest_promotion");
        assert_eq!(delivery.gates[0].state, AgentTaskGateStatus::Failed);
    }
}

#[test]
fn moving_base_blocker_carries_its_message_paths_and_continuation() {
    let delivery = CookDelivery::from_run_metadata(&json!({
        "latest_promotion": promotion("applied", passed_gate()),
        "cook_moving_base_recovery": moving_base("base moved during promotion", 1),
    }))
    .expect("delivery");
    assert_eq!(delivery.base_movements, 1);
    let blocker = delivery.blocker.expect("blocker");
    assert_eq!(blocker.message, "base moved during promotion");
    assert_eq!(blocker.paths, vec!["src/lib.rs".to_string()]);
    assert_eq!(
        blocker.next_action.as_deref(),
        Some("homeboy agent-task cook-continue run-1")
    );

    // An exhausted or foreign destination is never offered a continuation.
    let foreign = CookDelivery::from_run_metadata(&json!({
        "cook_moving_base_recovery": moving_base(
            "remote branch differs from the exact promoted candidate",
            3,
        ),
    }))
    .expect("delivery");
    assert_eq!(
        foreign.blocker.and_then(|blocker| blocker.next_action),
        None
    );
}

#[test]
fn publication_refusals_keep_the_writer_code_and_message() {
    let delivery = CookDelivery::from_run_metadata(&json!({
        "latest_promotion": promotion("applied", passed_gate()),
        "manual_finalization_failure": {
            "status": "failed",
            "phase": "publication",
            "error": { "code": "git.push_rejected", "message": "push was rejected by the remote" }
        },
    }))
    .expect("delivery");
    let blocker = delivery.blocker.expect("blocker");
    assert_eq!(blocker.code, CookDeliveryBlockerCode::PublishRefused);
    assert_eq!(blocker.message, "push was rejected by the remote");
    assert_eq!(blocker.cause.as_deref(), Some("git.push_rejected"));
    assert_eq!(blocker.evidence, "/manual_finalization_failure");
}

#[test]
fn published_delivery_reads_candidate_destination_gates_and_publication() {
    let delivery = CookDelivery::from_run_metadata(&json!({
        "latest_promotion": promotion("applied", passed_gate()),
        "cook_finalization": published("draft_published"),
    }))
    .expect("delivery");
    let candidate = delivery.candidate.expect("candidate");
    assert_eq!(candidate.promotion, AgentTaskPromotionStatus::Applied);
    assert_eq!(candidate.fingerprint.as_deref(), Some("fingerprint-sha"));
    assert_eq!(candidate.patch_sha.as_deref(), Some("patchsha"));
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
fn tolerated_spellings_read_as_their_canonical_values() {
    // Older records carry only promotion history, "passed" gates and a
    // force-with-lease receipt for the pushed head.
    let delivery = CookDelivery::from_run_metadata(&json!({
        "promotions": [
            { "status": "verification_pending" },
            { "status": "applied", "gate_results": [{ "name": "check", "status": "passed" }] }
        ],
        "cook_finalization": { "status": "review_ready", "pull_request_url": "https://example.invalid/pr/7" },
        "cook_force_with_lease_receipt": { "remote_sha": "lease-sha" },
    }))
    .expect("delivery");
    assert_eq!(delivery.state, CookDeliveryState::Published);
    assert_eq!(
        delivery.promotion(),
        Some(AgentTaskPromotionStatus::Applied)
    );
    assert_eq!(delivery.gates[0].id.as_deref(), Some("check"));
    assert_eq!(delivery.gates[0].state, AgentTaskGateStatus::Succeeded);
    let publication = delivery.publication.expect("publication");
    assert_eq!(
        publication.pr_identity().as_deref(),
        Some("https://example.invalid/pr/7")
    );
    assert_eq!(publication.pushed_sha.as_deref(), Some("lease-sha"));

    // The first post-apply checkpoint keeps the candidate identity when the
    // latest report omits it.
    let mut latest = promotion("applied", passed_gate());
    latest["provenance"]["candidate"] = Value::Null;
    let recovered = CookDelivery::from_run_metadata(&json!({
        "latest_promotion": latest,
        "cook_recovery_source_checkpoint": promotion("verification_pending", json!([])),
    }))
    .expect("delivery");
    assert_eq!(
        recovered
            .candidate
            .and_then(|candidate| candidate.fingerprint)
            .as_deref(),
        Some("fingerprint-sha")
    );
}

#[test]
fn a_pr_identity_without_a_publish_status_is_not_published() {
    for status in ["completed", "failed", "pending"] {
        let publication = publication_receipt(&json!({ "status": status, "pr_number": 42 }));
        assert!(!publication.published, "{status}");
        assert_eq!(publication.pr_identity().as_deref(), Some("42"), "{status}");
    }
}

#[test]
fn runs_without_delivery_evidence_have_no_delivery() {
    for metadata in [
        json!({}),
        json!({ "latest_promotion": null, "cook_finalization": null }),
        json!({ "latest_promotion": { "status": "dry_run" } }),
    ] {
        assert_eq!(
            CookDelivery::from_run_metadata(&metadata),
            None,
            "{metadata}"
        );
    }
}
