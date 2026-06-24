use std::collections::{BTreeMap, BTreeSet};

use homeboy::cli_surface::current_command_surface;
use homeboy::command_contract::{
    registered_command_json_family, CommandJsonFamily, PUBLIC_OUTPUT_VARIANT_CONTRACTS,
};
use homeboy::commands::bench::BenchOutput;
use homeboy::commands::extension::{ExtensionDetail, ExtensionOutput};
use homeboy::commands::rig::RigCommandOutput;
use homeboy::commands::runs::RunsOutput;
use homeboy::commands::utils::response::cli_response_for_json_result;
use homeboy::core::code_audit::report::{
    AuditChangedSinceSummary, AuditCommandOutput, AuditFixability, AuditSummaryGroup,
    AuditSummaryOutput, FixabilityKindBreakdown,
};
use homeboy::core::code_audit::FindingConfidence;
use homeboy::core::extension::lint::LintCommandOutput;
use homeboy::core::extension::test::{
    RawTestOutput, TestCommandOutput, TestCounts, TestSummaryOutput,
};
use homeboy::core::extension::{
    PhaseFailure, PhaseFailureCategory, PhaseReport, PhaseStatus, StructuredSidecarDeclaration,
    VerificationPhase,
};
use homeboy::core::finding::{FindingProducerSummary, FindingSource, HomeboyFinding};
use homeboy::core::plan::HomeboyPlan;
use homeboy::core::review::{
    ReviewArtifact, ReviewArtifactCommand, ReviewCommandOutput, ReviewStage, ReviewSummary,
};
use serde::Serialize;
use serde_json::{json, Value};

const REQUIRED_QUALITY_COMMAND_FIXTURES: &[&str] = &["audit", "lint", "review", "test"];
const REQUIRED_OPS_VARIANT_COMMANDS: &[&str] = &["db", "deploy"];

struct OutputContractScenario {
    command: &'static str,
    fixture: &'static str,
    exit_code: i32,
    payload: fn() -> Value,
}

struct VariantContract {
    name: &'static str,
    value: Value,
}

#[test]
fn visible_quality_commands_have_declared_golden_json_contract_fixtures() {
    let surface = current_command_surface();
    let covered: BTreeSet<_> = quality_output_contract_scenarios()
        .iter()
        .map(|scenario| scenario.command)
        .collect();

    for command in REQUIRED_QUALITY_COMMAND_FIXTURES {
        assert!(
            surface.contains_path(&[*command]),
            "required quality output contract command is not visible in the CLI surface: {command}"
        );
        assert!(
            covered.contains(command),
            "missing golden JSON output contract fixture for visible quality command: {command}"
        );
    }
}

#[test]
fn public_output_variant_contracts_cover_known_ops_command_families() {
    let surface = current_command_surface();
    let covered: BTreeSet<_> = PUBLIC_OUTPUT_VARIANT_CONTRACTS
        .iter()
        .map(|contract| contract.command)
        .collect();

    for command in REQUIRED_OPS_VARIANT_COMMANDS {
        assert!(
            surface.contains_path(&[*command]),
            "required ops output variant command is not visible in the CLI surface: {command}"
        );
        assert_eq!(
            registered_command_json_family(command),
            Some(CommandJsonFamily::Ops),
            "required output variant command should stay routed through the ops JSON family: {command}"
        );
        assert!(
            covered.contains(command),
            "missing public output variant contract for ops command family: {command}"
        );
    }
}

#[test]
fn quality_command_golden_json_contract_fixtures_match_typed_payloads() {
    for scenario in quality_output_contract_scenarios() {
        let actual = enveloped_json((scenario.payload)(), scenario.exit_code);
        let expected: Value = serde_json::from_str(scenario.fixture)
            .unwrap_or_else(|err| panic!("{} fixture should parse: {err}", scenario.command));

        assert_eq!(actual, expected, "{} golden JSON drifted", scenario.command);
    }
}

#[test]
fn runs_rig_and_bench_output_variants_have_unambiguous_contracts() {
    assert!(
        [
            std::any::type_name::<RunsOutput>(),
            std::any::type_name::<RigCommandOutput>(),
            std::any::type_name::<BenchOutput>(),
        ]
        .iter()
        .all(|output_type| output_type.starts_with("homeboy::commands::")),
        "contract test should stay anchored to public command output enums"
    );

    assert_unique_variant_signatures(
        "runs",
        vec![
            variant_contract("list", json!({ "command": "runs.list", "runs": [] })),
            variant_contract(
                "distribution",
                json!({ "command": "runs.distribution", "filters": {}, "fields": [] }),
            ),
            variant_contract(
                "latest_run",
                json!({ "command": "runs.latest-run", "run": {} }),
            ),
            variant_contract("compare", json!({ "command": "runs.compare", "rows": [] })),
            variant_contract("show", json!({ "command": "runs.show", "run": {} })),
            variant_contract(
                "evidence",
                json!({
                    "command": "runs.evidence",
                    "run_id": "run-1",
                    "run": {},
                    "metadata": {},
                    "heartbeat": {},
                    "artifact_index": {},
                    "retention": {},
                    "failure": {},
                    "disk_budget": {},
                    "evidence_links": []
                }),
            ),
            variant_contract(
                "artifacts",
                json!({ "command": "runs.artifacts", "run_id": "run-1", "artifacts": [] }),
            ),
            variant_contract(
                "artifact_get",
                json!({
                    "command": "runs.artifact.get",
                    "run_id": "run-1",
                    "artifact_id": "summary",
                    "output_path": "summary.json"
                }),
            ),
            variant_contract(
                "artifact_cleanup_downloads",
                json!({
                    "command": "runs.artifact.cleanup-downloads",
                    "dry_run": true,
                    "root": "/tmp/homeboy",
                    "removed": false,
                    "file_count": 0,
                    "directory_count": 0,
                    "size_bytes": 0,
                    "paths": []
                }),
            ),
            variant_contract(
                "artifact_cleanup_persisted",
                json!({
                    "command": "runs.artifact.cleanup-persisted",
                    "dry_run": true,
                    "artifact_root": "/tmp/homeboy/artifacts",
                    "older_than_days": 30,
                    "inspected_count": 0,
                    "planned_record_count": 0,
                    "planned_file_count": 0,
                    "planned_directory_count": 0,
                    "planned_size_bytes": 0,
                    "removed_record_count": 0,
                    "removed_file_count": 0,
                    "removed_directory_count": 0,
                    "removed_size_bytes": 0,
                    "skipped_count": 0,
                    "rows": []
                }),
            ),
            variant_contract(
                "findings",
                json!({ "command": "runs.findings", "findings": [] }),
            ),
            variant_contract(
                "finding",
                json!({ "command": "runs.finding", "finding": {} }),
            ),
            variant_contract(
                "latest_finding",
                json!({ "command": "runs.latest-finding", "run": {}, "finding": {} }),
            ),
            variant_contract(
                "bench_compare",
                json!({
                    "command": "runs.bench-compare",
                    "from_run": {},
                    "to_run": {},
                    "comparisons": [],
                    "missing": []
                }),
            ),
            variant_contract(
                "reconcile",
                json!({ "command": "runs.reconcile", "stale_runs": [] }),
            ),
            variant_contract(
                "export",
                json!({ "command": "runs.export", "output": "bundle", "manifest": {} }),
            ),
            variant_contract(
                "import",
                json!({ "command": "runs.import", "input": "bundle", "imported": {} }),
            ),
            variant_contract(
                "import_from_gh_actions",
                json!({ "command": "runs.import-gh-actions", "imported": {} }),
            ),
            variant_contract(
                "query",
                json!({
                    "command": "runs.query",
                    "filters": {},
                    "select": [],
                    "matched_artifact_count": 0,
                    "skipped_artifact_count": 0
                }),
            ),
            variant_contract(
                "drift",
                json!({
                    "command": "runs.drift",
                    "filters": {},
                    "metric": "$.status",
                    "threshold": 0.5,
                    "window_observations": 0,
                    "window_missing_rows": 0,
                    "values": []
                }),
            ),
            variant_contract(
                "loop_sync",
                json!({
                    "command": "runs.loop-sync",
                    "dry_run": true,
                    "archive_root": "/tmp/loop-archives",
                    "run_id": null,
                    "synced_artifacts": [],
                    "triage": {}
                }),
            ),
        ],
    );

    assert_unique_variant_signatures(
        "rig",
        vec![
            variant_contract("list", json!({ "command": "rig.list", "rigs": [] })),
            variant_contract("show", json!({ "command": "rig.show", "rig": {} })),
            variant_contract("up", json!({ "command": "rig.up", "steps": [] })),
            variant_contract("check", json!({ "command": "rig.check", "checks": [] })),
            variant_contract("down", json!({ "command": "rig.down", "steps": [] })),
            variant_contract("repair", json!({ "command": "rig.repair", "steps": [] })),
            variant_contract("sync", json!({ "command": "rig.sync", "stacks": [] })),
            variant_contract("status", json!({ "command": "rig.status", "rigs": [] })),
            variant_contract(
                "install",
                json!({
                    "command": "rig.install",
                    "source": "fixtures",
                    "package_path": ".",
                    "linked": false,
                    "installed": [],
                    "installed_stacks": []
                }),
            ),
            variant_contract("update", json!({ "command": "rig.update", "updated": [] })),
            variant_contract(
                "sources_list",
                json!({ "command": "rig.sources.list", "sources": [] }),
            ),
            variant_contract(
                "sources_remove",
                json!({ "command": "rig.sources.remove", "removed": true }),
            ),
            variant_contract(
                "app_install",
                json!({ "command": "rig.app.install", "apps": [] }),
            ),
            variant_contract("runs", json!({ "command": "runs.list", "runs": [] })),
        ],
    );

    assert_unique_variant_signatures(
        "bench",
        vec![
            variant_contract(
                "single",
                json!({
                    "passed": true,
                    "status": "passed",
                    "component": "homeboy",
                    "exit_code": 0,
                    "iterations": 10
                }),
            ),
            variant_contract(
                "comparison",
                json!({
                    "comparison": "cross_rig",
                    "passed": true,
                    "component": "homeboy",
                    "exit_code": 0,
                    "iterations": 10,
                    "rigs": [],
                    "diff": {},
                    "reports": {}
                }),
            ),
            variant_contract(
                "comparison_summary",
                json!({
                    "comparison": "cross_rig",
                    "summary_only": true,
                    "passed": true,
                    "component": "homeboy",
                    "exit_code": 0,
                    "iterations": 10,
                    "rigs": []
                }),
            ),
            variant_contract(
                "list",
                json!({
                    "component": "homeboy",
                    "component_id": "homeboy",
                    "scenarios": [],
                    "count": 0
                }),
            ),
            variant_contract("observation", json!({ "command": "runs.list", "runs": [] })),
        ],
    );
}

#[test]
fn extension_show_output_contracts_use_top_level_structured_sidecars() {
    let output = typed_output_value(ExtensionOutput::Show {
        extension: ExtensionDetail {
            id: "sample-extension".to_string(),
            name: "Sample Extension".to_string(),
            version: "1.0.0".to_string(),
            description: None,
            author: None,
            homepage: None,
            source_url: None,
            runtime: "platform".to_string(),
            runtime_requirements: None,
            has_setup: None,
            has_ready_check: None,
            ready: true,
            ready_reason: None,
            ready_detail: None,
            linked: false,
            path: "/extensions/sample-extension".to_string(),
            source_revision: None,
            cli: None,
            actions: Vec::new(),
            inputs: Vec::new(),
            settings: Vec::new(),
            structured_sidecars: vec![StructuredSidecarDeclaration {
                name: "findings".to_string(),
                path: "findings.json".to_string(),
                schema_version: Some("1".to_string()),
                producer: Some("lint".to_string()),
            }],
            requires: None,
        },
    });

    assert_eq!(
        output["extension"]["structured_sidecars"],
        json!([{ "name": "findings", "path": "findings.json", "schema_version": "1", "producer": "lint" }])
    );
    assert_eq!(output["extension"].get("lint"), None);
}

fn quality_output_contract_scenarios() -> Vec<OutputContractScenario> {
    vec![
        OutputContractScenario {
            command: "audit",
            fixture: include_str!("fixtures/output_contracts/quality/audit-summary.json"),
            exit_code: 1,
            payload: audit_summary_payload,
        },
        OutputContractScenario {
            command: "lint",
            fixture: include_str!("fixtures/output_contracts/quality/lint-findings.json"),
            exit_code: 1,
            payload: lint_findings_payload,
        },
        OutputContractScenario {
            command: "test",
            fixture: include_str!("fixtures/output_contracts/quality/test-summary.json"),
            exit_code: 1,
            payload: test_summary_payload,
        },
        OutputContractScenario {
            command: "review",
            fixture: include_str!("fixtures/output_contracts/quality/review-artifact.json"),
            exit_code: 1,
            payload: review_artifact_payload,
        },
    ]
}

fn enveloped_json(payload: Value, exit_code: i32) -> Value {
    let result = Ok(payload);
    serde_json::to_value(cli_response_for_json_result(&result, exit_code))
        .expect("CLI response should serialize")
}

fn typed_output_value<T: Serialize>(output: T) -> Value {
    serde_json::to_value(output).expect("command output should serialize")
}

fn variant_contract(name: &'static str, value: Value) -> VariantContract {
    VariantContract { name, value }
}

fn assert_unique_variant_signatures(group: &str, contracts: Vec<VariantContract>) {
    let mut signatures = BTreeMap::<String, &'static str>::new();

    for contract in contracts {
        let signature = variant_signature(&contract.value);
        if let Some(existing) = signatures.insert(signature.clone(), contract.name) {
            panic!(
                "{group} output variants `{existing}` and `{}` share ambiguous signature `{signature}`",
                contract.name
            );
        }
    }
}

fn variant_signature(value: &Value) -> String {
    if let Some(command) = value.get("command").and_then(Value::as_str) {
        return format!("command={command}");
    }

    if let Some(comparison) = value.get("comparison").and_then(Value::as_str) {
        return if value
            .get("summary_only")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            format!("comparison={comparison};summary_only=true")
        } else {
            format!("comparison={comparison};summary_only=false")
        };
    }

    let keys = value
        .as_object()
        .expect("variant contract payload should be a JSON object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");

    format!("shape={keys}")
}

fn audit_summary_payload() -> Value {
    typed_output_value(audit_summary_output())
}

fn lint_findings_payload() -> Value {
    typed_output_value(lint_findings_output())
}

fn test_summary_payload() -> Value {
    typed_output_value(test_summary_output())
}

fn review_artifact_payload() -> Value {
    typed_output_value(review_artifact_output())
}

fn audit_summary_output() -> AuditCommandOutput {
    let mut by_kind = BTreeMap::new();
    by_kind.insert(
        "god_file".to_string(),
        FixabilityKindBreakdown {
            total: 2,
            automated: 1,
            manual_only: 1,
        },
    );

    AuditCommandOutput::Summary(AuditSummaryOutput {
        alignment_score: Some(0.82),
        total_findings: 2,
        warnings: 1,
        info: 1,
        finding_groups: vec![AuditSummaryGroup {
            kind: "god_file".to_string(),
            count: 2,
            warnings: 1,
            info: 1,
            confidence: FindingConfidence::Heuristic,
            sample_files: vec!["src/large.rs".to_string(), "src/also-large.rs".to_string()],
            drilldown_command: "homeboy audit fixture --only god_file".to_string(),
        }],
        top_findings: vec![
            HomeboyFinding::builder("audit", "File exceeds the size threshold")
                .rule("god_file")
                .category("structural")
                .file("src/large.rs")
                .severity("warning")
                .metadata("convention", "structural")
                .metadata("suggestion", "Split the module into focused pieces")
                .metadata("confidence", FindingConfidence::Heuristic)
                .metadata("kind", "god_file")
                .build(),
        ],
        fixability: Some(AuditFixability {
            fixable_count: 2,
            automated_count: 1,
            manual_only_count: 1,
            by_kind,
        }),
        changed_since: Some(AuditChangedSinceSummary {
            introduced_findings: 1,
            contextual_findings: 1,
        }),
        baseline_filtering: None,
        unbaselined_findings: Vec::new(),
        extension_phase_timings: Vec::new(),
        exit_code: 1,
    })
}

fn lint_findings_output() -> LintCommandOutput {
    LintCommandOutput {
        passed: false,
        status: "failed".to_string(),
        component: "fixture".to_string(),
        exit_code: 1,
        phase: PhaseReport {
            phase: VerificationPhase::Lint,
            status: PhaseStatus::Failed,
            exit_code: Some(1),
            summary: "lint phase failed with 1 finding(s) across fixture-linter failed: 1"
                .to_string(),
        },
        failure: Some(PhaseFailure {
            phase: VerificationPhase::Lint,
            category: PhaseFailureCategory::Findings,
            summary: "1 lint finding(s) detected".to_string(),
        }),
        autofix: None,
        hints: Some(vec![
            "Run `homeboy lint fixture --fix` to apply safe fixes.".to_string(),
        ]),
        baseline_comparison: None,
        formatting_findings: None,
        findings: Some(vec![HomeboyFinding::builder(
            "fixture-linter",
            "Trailing whitespace",
        )
        .category("whitespace")
        .rule("trailing-whitespace")
        .file("src/lib.rs")
        .severity("warning")
        .fingerprint("lint:src/lib.rs:12:trailing-whitespace")
        .source(
            FindingSource::new("sidecar")
                .label("lint-findings")
                .path("lint-findings.json"),
        )
        .build()]),
        producer_summaries: vec![FindingProducerSummary::new("fixture-linter", "failed")
            .finding_count(1)
            .source(
                FindingSource::new("sidecar")
                    .label("lint-findings")
                    .path("lint-findings.json"),
            )],
        summary: None,
        self_check_capture: None,
        ci_context: None,
        extension_phase_timings: Vec::new(),
    }
}

fn test_summary_output() -> TestCommandOutput {
    TestCommandOutput {
        passed: false,
        status: "failed".to_string(),
        component: "fixture".to_string(),
        exit_code: 1,
        phase: Some(PhaseReport {
            phase: VerificationPhase::Test,
            status: PhaseStatus::Failed,
            exit_code: Some(1),
            summary: "test phase reported 1 failure(s) out of 3 test(s)".to_string(),
        }),
        failure: Some(PhaseFailure {
            phase: VerificationPhase::Test,
            category: PhaseFailureCategory::Findings,
            summary: "1 test failure(s) detected".to_string(),
        }),
        test_counts: Some(TestCounts::new(3, 2, 1, 0)),
        findings: Some(vec![HomeboyFinding::builder(
            "test",
            "expected stable JSON envelope",
        )
        .severity("error")
        .file("tests/output_contract.rs")
        .line(24)
        .metadata("test_name", "fixture::fails_contract")
        .build()]),
        coverage: None,
        baseline_comparison: None,
        analysis: None,
        autofix: None,
        hints: Some(vec![
            "Re-run: homeboy test fixture --json-summary".to_string()
        ]),
        drift: None,
        auto_fix_drift: None,
        test_scope: None,
        summary: Some(TestSummaryOutput {
            total: 3,
            passed: 2,
            failed: 1,
            skipped: 0,
            failures: Vec::new(),
            exit_code: 1,
        }),
        raw_output: Some(RawTestOutput {
            stdout_tail: "running 3 tests\nfixture::fails_contract FAILED".to_string(),
            stderr_tail: "assertion failed: stable envelope".to_string(),
            truncated: false,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_seen_bytes: 0,
            stdout_retained_bytes: 0,
            stderr_seen_bytes: 0,
            stderr_retained_bytes: 0,
            stdout_limit_bytes: 0,
            stderr_limit_bytes: 0,
        }),
        ci_context: None,
        extension_phase_timings: Vec::new(),
    }
}

fn review_artifact_output() -> ReviewCommandOutput {
    ReviewCommandOutput {
        command: "review".to_string(),
        plan: HomeboyPlan::default(),
        observation: None,
        artifact: ReviewArtifact {
            schema: "homeboy/review/v1".to_string(),
            component: "fixture".to_string(),
            status: "fail".to_string(),
            generated_at: "2026-05-24T00:00:00Z".to_string(),
            base_ref: "origin/main".to_string(),
            head_ref: "HEAD".to_string(),
            observation: None,
            commands: vec![ReviewArtifactCommand {
                name: "lint".to_string(),
                status: "fail".to_string(),
                exit_code: 1,
                summary: "1 lint finding(s) detected".to_string(),
                findings: vec![HomeboyFinding::builder("lint", "Trailing whitespace")
                    .fingerprint("lint:src/lib.rs:12:trailing-whitespace")
                    .build()],
                artifacts: Vec::new(),
            }],
        },
        summary: ReviewSummary {
            passed: false,
            status: "fail".to_string(),
            component: "fixture".to_string(),
            scope: "changed-since".to_string(),
            changed_since: Some("origin/main".to_string()),
            total_findings: 1,
            changed_file_count: Some(2),
            hints: vec!["Deep dive: homeboy lint fixture --changed-since origin/main".to_string()],
        },
        audit: skipped_review_stage("audit"),
        lint: skipped_review_stage("lint"),
        test: skipped_review_stage("test"),
        ci_profile: None,
    }
}

fn skipped_review_stage<T: Serialize>(stage: &str) -> ReviewStage<T> {
    ReviewStage {
        stage: stage.to_string(),
        ran: false,
        passed: true,
        exit_code: 0,
        finding_count: 0,
        hint: format!("Skipped {stage} in contract fixture"),
        skipped_reason: Some(
            "fixture keeps nested command payloads in their own golden files".to_string(),
        ),
        output: None,
    }
}
