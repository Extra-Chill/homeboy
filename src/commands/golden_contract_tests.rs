use serde::Serialize;
use std::path::Path;

use serde_json::{json, Value};

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/golden_json_contracts"
);

use super::bench::BenchOutput;
use super::deploy::{DeployCommandOutput, DeployOutput, MultiProjectDeployOutput};
use super::release::{BatchReleaseOutput, ReleaseCommandOutput, ReleaseOutput};
use super::runs::{
    DriftValue, QueryGroup, QueryRow, RunsDriftFilters, RunsDriftOutput, RunsQueryFilters,
    RunsRefsArtifactRef, RunsRefsFilters, RunsRefsOutput, RunsRefsRunRef, SkippedArtifactRow,
    TestRunsQueryOutput as RunsQueryOutput,
};
use super::runs::{
    RunDetail, RunSummary, RunsArtifactsOutput, RunsListOutput, RunsOutput, RunsShowOutput,
};
use super::stack::{
    StackInspectOutput, StackListOutput, StackShowOutput, StackStatusOutput, StackSummary,
};
use super::utils::response::CliResponse;
use crate::core::deploy::{
    ComponentDeployResult, ComponentStatus, DeployReason, DeploySummary, MultiDeploySummary,
    ProjectDeployResult,
};
use crate::core::extension::bench::{BenchCommandOutput, BenchListWorkflowResult};
use crate::core::observation::ArtifactRecord;
use crate::core::release::{
    BatchReleaseComponentResult, BatchReleaseResult, BatchReleaseSummary, ReleaseCommandResult,
    ReleasePlan, ReleaseSemverCommit, ReleaseSemverRecommendation,
};
use crate::core::stack::{
    GitRef, InspectCommit, InspectCommitDetails, InspectOutput, InspectPr, LocalState,
    StackPrEntry, StackSpec, StatusOutput, StatusPr,
};

#[test]
fn bench_command_json_contract_matches_golden_fixture() {
    assert_fixture(
        "bench_contract.json",
        json!({
            "scenarios": [
                scenario("bench single", BenchOutput::Single(BenchCommandOutput {
                    passed: true,
                    status: "passed".to_string(),
                    component: "homeboy".to_string(),
                    exit_code: 0,
                    iterations: 3,
                    artifacts: Vec::new(),
                    results: None,
                    budget_findings: Vec::new(),
                    gate_results: Vec::new(),
                    gate_failures: Vec::new(),
                    baseline_comparison: None,
                    hints: None,
                    rig_state: None,
                    failure: None,
                    diagnostics: Vec::new(),
                    ci_context: None,
                    persisted_run: None,
                })),
                scenario("bench list", BenchOutput::List(BenchListWorkflowResult {
                    component: "homeboy".to_string(),
                    component_id: "homeboy".to_string(),
                    scenarios: Vec::new(),
                    count: 0,
                })),
            ]
        }),
    );
}

#[test]
fn runs_command_json_contract_matches_golden_fixture() {
    assert_fixture(
        "runs_contract.json",
        json!({
            "scenarios": [
                scenario("runs list", RunsOutput::List(RunsListOutput {
                    command: "runs.list",
                    runs: vec![run_summary()],
                })),
                scenario("runs refs json", RunsOutput::Refs(RunsRefsOutput {
                    command: "runs.refs",
                    filters: RunsRefsFilters {
                        component_id: Some("homeboy".to_string()),
                        kind: Some("bench".to_string()),
                        rig: Some("contract-rig".to_string()),
                        status: Some("pass".to_string()),
                        since: None,
                        limit: 50,
                        artifact_kinds: Vec::new(),
                        aggregate_artifact_kinds: Vec::new(),
                    },
                    run_count: 1,
                    artifact_count: 1,
                    aggregate_artifact_count: 1,
                    runs: vec![RunsRefsRunRef {
                        run_id: "run-contract-1".to_string(),
                        ref_id: "homeboy://run/run-contract-1".to_string(),
                        kind: "bench".to_string(),
                        status: "pass".to_string(),
                        started_at: "2026-05-24T00:00:00Z".to_string(),
                        finished_at: Some("2026-05-24T00:01:00Z".to_string()),
                        component_id: Some("homeboy".to_string()),
                        rig_id: Some("contract-rig".to_string()),
                        git_sha: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
                        evidence_commands: homeboy::core::observation::RunEvidenceCommands {
                            evidence_command: "homeboy runs evidence run-contract-1".to_string(),
                            artifacts_command: "homeboy runs artifacts run-contract-1".to_string(),
                        },
                    }],
                    artifacts: vec![runs_refs_artifact_record()],
                    aggregate_artifacts: vec![runs_refs_artifact_record()],
                })),
                scenario("runs show", RunsOutput::Show(RunsShowOutput {
                    command: "runs.show",
                    run: RunDetail {
                        summary: run_summary(),
                        homeboy_version: Some("0.197.11".to_string()),
                        metadata: json!({ "scenario": "contract", "score": 0.98 }),
                        artifacts: vec![artifact_record()],
                    },
                })),
                scenario("runs artifacts", RunsOutput::Artifacts(RunsArtifactsOutput {
                    command: "runs.artifacts",
                    run_id: "run-contract-1".to_string(),
                    artifacts: vec![artifact_record()],
                })),
                scenario("runs query json", RunsOutput::Query(RunsQueryOutput {
                    command: "runs.query",
                    filters: RunsQueryFilters {
                        component_id: Some("homeboy".to_string()),
                        kind: Some("bench".to_string()),
                        since: Some("7d".to_string()),
                        limit: 200,
                    },
                    select: vec!["$.metrics.ready_ms".to_string()],
                    group_by: Some("$.variant".to_string()),
                    matched_artifact_count: 1,
                    skipped_artifact_count: 1,
                    skipped_artifacts: vec![SkippedArtifactRow {
                        run_id: "run-contract-1".to_string(),
                        artifact_id: "artifact-log".to_string(),
                        artifact_kind: "log".to_string(),
                        artifact_type: "file".to_string(),
                        path: "artifacts/output.log".to_string(),
                        reason: "not JSON".to_string(),
                    }],
                    rows: vec![QueryRow {
                        run_id: "run-contract-1".to_string(),
                        artifact_kind: "summary".to_string(),
                        values: vec![json!(123.4)],
                    }],
                    groups: vec![QueryGroup {
                        group: "candidate".to_string(),
                        count: 1,
                    }],
                    table: None,
                    csv: None,
                })),
                scenario("runs drift json", RunsOutput::Drift(RunsDriftOutput {
                    command: "runs.drift",
                    filters: RunsDriftFilters {
                        component_id: Some("homeboy".to_string()),
                        kind: Some("bench".to_string()),
                        window: "7d".to_string(),
                        baseline: Some("30d".to_string()),
                    },
                    metric: "$.variant".to_string(),
                    threshold: 0.5,
                    window_observations: 10,
                    window_missing_rows: 0,
                    baseline_observations: Some(40),
                    baseline_missing_rows: Some(2),
                    values: vec![DriftValue {
                        value: "candidate".to_string(),
                        window_count: 7,
                        window_share: 0.7,
                        dominant: true,
                        baseline_share: Some(0.25),
                        share_delta: Some(0.45),
                    }],
                    table: None,
                })),
            ]
        }),
    );
}

#[test]
fn release_command_json_contract_matches_golden_fixture() {
    assert_fixture(
        "release_contract.json",
        json!({
            "scenarios": [
                scenario("release dry-run single", ReleaseCommandOutput::Single(ReleaseOutput {
                    result: release_result("homeboy", "minor", true, Some(release_plan())),
                })),
                scenario("release dry-run batch", ReleaseCommandOutput::Batch(BatchReleaseOutput {
                    result: BatchReleaseResult {
                        results: vec![BatchReleaseComponentResult {
                            component_id: "homeboy".to_string(),
                            status: "planned".to_string(),
                            error: None,
                            result: Some(release_result("homeboy", "minor", true, Some(release_plan()))),
                        }],
                        summary: BatchReleaseSummary {
                            total: 1,
                            released: 0,
                            skipped: 0,
                            failed: 0,
                        },
                    },
                })),
            ]
        }),
    );
}

#[test]
fn deploy_command_json_contract_matches_golden_fixture() {
    assert_fixture(
        "deploy_contract.json",
        json!({
            "scenarios": [
                scenario("deploy dry-run single", DeployCommandOutput::Single(DeployOutput {
                    command: "deploy.run".to_string(),
                    project_id: "production".to_string(),
                    all: false,
                    outdated: false,
                    behind_upstream: false,
                    dry_run: true,
                    check: false,
                    force: false,
                    results: vec![component_deploy_result("homeboy")],
                    summary: deploy_summary(1, 0, 0, 1),
                })),
                scenario("deploy dry-run multi-project", DeployCommandOutput::Multi(MultiProjectDeployOutput {
                    command: "deploy.multi".to_string(),
                    component_ids: vec!["homeboy".to_string()],
                    projects: vec![ProjectDeployResult {
                        project_id: "production".to_string(),
                        status: "planned".to_string(),
                        error: None,
                        results: vec![component_deploy_result("homeboy")],
                        summary: deploy_summary(1, 0, 0, 1),
                    }],
                    summary: MultiDeploySummary {
                        total_projects: 1,
                        succeeded: 0,
                        failed: 0,
                        skipped: 0,
                        planned: 1,
                    },
                    dry_run: true,
                    check: false,
                    force: false,
                })),
            ]
        }),
    );
}

#[test]
fn stack_command_json_contract_matches_golden_fixture() {
    assert_fixture(
        "stack_contract.json",
        json!({
            "scenarios": [
                scenario("stack list", StackListOutput {
                    command: "stack.list",
                    stacks: vec![StackSummary {
                        id: "combined-fixes".to_string(),
                        description: "Combined fixes branch".to_string(),
                        component: "homeboy".to_string(),
                        component_path: "/work/homeboy".to_string(),
                        base: "origin/main".to_string(),
                        target: "origin/dev/combined-fixes".to_string(),
                        pr_count: 1,
                    }],
                }),
                scenario("stack show", StackShowOutput {
                    command: "stack.show",
                    stack: stack_spec(),
                }),
                scenario("stack status", StackStatusOutput {
                    command: "stack.status",
                    report: StatusOutput {
                        stack_id: "combined-fixes".to_string(),
                        component_path: "/work/homeboy".to_string(),
                        base: "origin/main".to_string(),
                        target: "dev/combined-fixes".to_string(),
                        target_ahead: Some(2),
                        target_behind: Some(0),
                        prs: vec![StatusPr {
                            repo: "Extra-Chill/homeboy".to_string(),
                            number: 2753,
                            note: Some("contract coverage".to_string()),
                            title: Some("Add golden JSON contract fixtures".to_string()),
                            url: Some("https://github.com/Extra-Chill/homeboy/pull/2753".to_string()),
                            upstream_state: Some("OPEN".to_string()),
                            review_decision: Some("REVIEW_REQUIRED".to_string()),
                            merged_at: None,
                            local_state: LocalState::Applied,
                            candidate_for_drop: false,
                            error: None,
                        }],
                        merged_count: 0,
                        success: true,
                    },
                }),
                scenario("stack inspect", StackInspectOutput {
                    command: "stack.inspect",
                    report: InspectOutput {
                        component_id: "homeboy".to_string(),
                        path: "/work/homeboy".to_string(),
                        branch: "fix/contract".to_string(),
                        base: "origin/main".to_string(),
                        base_auto_detected: false,
                        commits: vec![InspectCommit {
                            commit: InspectCommitDetails {
                                sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                                short_sha: "aaaaaaaa".to_string(),
                                subject: "test: pin command JSON contract".to_string(),
                                author: "Homeboy Bot".to_string(),
                                date: "2026-05-24T00:00:00Z".to_string(),
                            },
                            pr: Some(InspectPr {
                                number: 2753,
                                state: "OPEN".to_string(),
                                title: "Add golden JSON contract fixtures".to_string(),
                                url: "https://github.com/Extra-Chill/homeboy/pull/2753".to_string(),
                            }),
                            pr_lookup_note: None,
                        }],
                        merged_count: 0,
                        success: true,
                    },
                }),
            ]
        }),
    );
}

fn scenario<T: Serialize>(name: &str, data: T) -> Value {
    json!({
        "scenario": name,
        "payload": serde_json::to_value(CliResponse::success(data)).expect("serialize CLI envelope"),
    })
}

fn assert_fixture(name: &str, actual: Value) {
    let path = Path::new(FIXTURE_ROOT).join(name);
    let expected = std::fs::read_to_string(&path).expect("golden fixture exists");
    let expected: Value = serde_json::from_str(&expected).expect("golden fixture is valid JSON");
    assert_eq!(actual, expected);
}

fn run_summary() -> RunSummary {
    RunSummary {
        id: "run-contract-1".to_string(),
        kind: "bench".to_string(),
        status: "passed".to_string(),
        started_at: "2026-05-24T00:00:00Z".to_string(),
        finished_at: Some("2026-05-24T00:01:00Z".to_string()),
        component_id: Some("homeboy".to_string()),
        rig_id: Some("contract-rig".to_string()),
        git_sha: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
        command: Some("homeboy bench --format=json".to_string()),
        cwd: Some("/work/homeboy".to_string()),
        status_note: Some("fixture".to_string()),
        artifact_index: None,
    }
}

fn artifact_record() -> ArtifactRecord {
    ArtifactRecord {
        id: "artifact-summary".to_string(),
        run_id: "run-contract-1".to_string(),
        kind: "summary".to_string(),
        artifact_type: "file".to_string(),
        path: "artifacts/summary.json".to_string(),
        url: Some("https://example.test/artifacts/summary.json".to_string()),
        public_url: None,
        viewer_url: None,
        viewer_links: Vec::new(),
        sha256: Some(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        ),
        size_bytes: Some(1234),
        mime: Some("application/json".to_string()),
        metadata_json: serde_json::json!({}),
        created_at: "2026-05-24T00:01:00Z".to_string(),
    }
}

fn runs_refs_artifact_record() -> RunsRefsArtifactRef {
    RunsRefsArtifactRef {
        run_id: "run-contract-1".to_string(),
        artifact_id: "artifact-aggregate".to_string(),
        ref_id: "homeboy://run/run-contract-1/artifact/artifact-aggregate".to_string(),
        kind: "trace_aggregate".to_string(),
        artifact_type: "file".to_string(),
        path: "artifacts/aggregate.json".to_string(),
        url: None,
        mime: Some("application/json".to_string()),
        size_bytes: Some(1234),
        sha256: Some(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
        ),
        get_command: "homeboy runs artifact get run-contract-1 artifact-aggregate".to_string(),
    }
}

fn release_plan() -> ReleasePlan {
    ReleasePlan::new(
        "homeboy",
        true,
        Vec::new(),
        Some(ReleaseSemverRecommendation {
            latest_tag: Some("v0.197.10".to_string()),
            range: "v0.197.10..HEAD".to_string(),
            commits: vec![ReleaseSemverCommit {
                sha: "aaaaaaaa".to_string(),
                subject: "feat: add contract fixtures".to_string(),
                commit_type: "feat".to_string(),
                breaking: false,
            }],
            recommended_bump: Some("minor".to_string()),
            requested_bump: "minor".to_string(),
            is_underbump: false,
            reasons: vec!["feat commit requires a minor release".to_string()],
        }),
        vec!["dry run only".to_string()],
        vec!["review generated plan before releasing".to_string()],
    )
}

fn release_result(
    component_id: &str,
    bump_type: &str,
    dry_run: bool,
    plan: Option<ReleasePlan>,
) -> ReleaseCommandResult {
    ReleaseCommandResult {
        component_id: component_id.to_string(),
        status: if dry_run { "planned" } else { "released" }.to_string(),
        bump_type: bump_type.to_string(),
        dry_run,
        releasable_commits: 1,
        new_version: Some("0.198.0".to_string()),
        tag: Some("v0.198.0".to_string()),
        skipped_reason: None,
        plan,
        run: None,
        deployment: None,
        release_summary: if dry_run {
            vec![
                "No release commit created".to_string(),
                "No tag created".to_string(),
                "No GitHub Release created".to_string(),
            ]
        } else {
            Vec::new()
        },
    }
}

fn component_deploy_result(id: &str) -> ComponentDeployResult {
    ComponentDeployResult {
        id: id.to_string(),
        status: "planned".to_string(),
        deploy_reason: Some(DeployReason::ExplicitlySelected),
        component_status: Some(ComponentStatus::NeedsUpdate),
        local_version: Some("0.198.0".to_string()),
        remote_version: Some("0.197.11".to_string()),
        local_path: Some("/work/homeboy".to_string()),
        git_branch: Some("main".to_string()),
        git_head: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
        upstream_branch: Some("origin/main".to_string()),
        upstream_head: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
        is_worktree: Some(false),
        behind_upstream: None,
        warnings: Vec::new(),
        error: None,
        artifact_path: Some("target/dist/homeboy.tar.gz".to_string()),
        artifact_inputs: Vec::new(),
        remote_path: Some("/srv/homeboy".to_string()),
        build_exit_code: None,
        deploy_exit_code: None,
        release_state: None,
        deployed_ref: Some("v0.198.0".to_string()),
    }
}

fn deploy_summary(total: u32, succeeded: u32, failed: u32, skipped: u32) -> DeploySummary {
    DeploySummary {
        total,
        succeeded,
        failed,
        skipped,
    }
}

fn stack_spec() -> StackSpec {
    StackSpec {
        id: "combined-fixes".to_string(),
        description: "Combined fixes branch".to_string(),
        component: "homeboy".to_string(),
        component_path: "/work/homeboy".to_string(),
        base: GitRef {
            remote: "origin".to_string(),
            branch: "main".to_string(),
        },
        target: GitRef {
            remote: "origin".to_string(),
            branch: "dev/combined-fixes".to_string(),
        },
        prs: vec![StackPrEntry {
            repo: "Extra-Chill/homeboy".to_string(),
            number: 2753,
            note: Some("contract coverage".to_string()),
        }],
    }
}
