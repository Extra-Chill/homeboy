use std::collections::{BTreeMap, BTreeSet};

use homeboy::cli_surface::current_command_surface;
use homeboy::commands::review::{
    ReviewArtifact, ReviewArtifactCommand, ReviewCommandOutput, ReviewStage, ReviewSummary,
};
use homeboy::commands::utils::response::cli_response_for_json_result;
use homeboy::core::code_audit::report::{
    AuditChangedSinceSummary, AuditCommandOutput, AuditFixability, AuditSummaryFinding,
    AuditSummaryGroup, AuditSummaryOutput, FixabilityKindBreakdown,
};
use homeboy::core::code_audit::{AuditFinding, FindingConfidence, Severity};
use homeboy::core::extension::lint::{LintCommandOutput, LintFinding};
use homeboy::core::extension::test::{
    FailedTest, RawTestOutput, TestCommandOutput, TestCounts, TestSummaryOutput,
};
use homeboy::core::extension::{
    PhaseFailure, PhaseFailureCategory, PhaseReport, PhaseStatus, VerificationPhase,
};
use homeboy::core::plan::HomeboyPlan;
use serde::Serialize;
use serde_json::{json, Value};

const REQUIRED_QUALITY_COMMAND_FIXTURES: &[&str] = &["audit", "lint", "review", "test"];

struct OutputContractScenario {
    command: &'static str,
    fixture: &'static str,
    exit_code: i32,
    payload: fn() -> Value,
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
fn quality_command_golden_json_contract_fixtures_match_typed_payloads() {
    for scenario in quality_output_contract_scenarios() {
        let actual = enveloped_json((scenario.payload)(), scenario.exit_code);
        let expected: Value = serde_json::from_str(scenario.fixture)
            .unwrap_or_else(|err| panic!("{} fixture should parse: {err}", scenario.command));

        assert_eq!(actual, expected, "{} golden JSON drifted", scenario.command);
    }
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
        top_findings: vec![AuditSummaryFinding {
            file: "src/large.rs".to_string(),
            convention: "structural".to_string(),
            kind: AuditFinding::GodFile,
            confidence: FindingConfidence::Heuristic,
            severity: Severity::Warning,
            description: "File exceeds the size threshold".to_string(),
            suggestion: "Split the module into focused pieces".to_string(),
        }],
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
            summary: "lint phase reported 1 finding(s)".to_string(),
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
        lint_findings: Some(vec![LintFinding {
            id: "lint:src/lib.rs:12:trailing-whitespace".to_string(),
            message: "Trailing whitespace".to_string(),
            category: "whitespace".to_string(),
            tool: Some("fixture-linter".to_string()),
            file: Some("src/lib.rs".to_string()),
            severity: Some("warning".to_string()),
            extra: BTreeMap::new(),
        }]),
        summary: None,
        ci_context: None,
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
        failed_tests: Some(vec![FailedTest {
            name: "fixture::fails_contract".to_string(),
            detail: Some("expected stable JSON envelope".to_string()),
            location: Some("tests/output_contract.rs:24".to_string()),
        }]),
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
        }),
        ci_context: None,
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
                findings: vec![json!({
                    "id": "lint:src/lib.rs:12:trailing-whitespace",
                    "message": "Trailing whitespace"
                })],
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
