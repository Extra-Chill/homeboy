//! Review command — scoped audit + lint + test umbrella.
//!
//! `homeboy review --changed-since=<ref>` runs the same scoped checks a CI
//! reviewer would run on a PR diff, fanning out to the existing
//! `audit`, `lint`, and `test` commands and collapsing their structured
//! results into a single consolidated report.
//!
//! The umbrella is deliberately thin: scoping logic lives in the underlying
//! commands (and in `core/git/changes.rs::get_files_changed_since`). Review
//! orchestrates ordering, short-circuits on empty changesets, and assembles
//! the consolidated output envelope.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/1500

use chrono::Utc;
use clap::Args;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

use homeboy::code_audit::AuditCommandOutput;
use homeboy::extension::lint::LintCommandOutput;
use homeboy::extension::test::TestCommandOutput;
use homeboy::git;

use super::parse_key_val;
use super::utils::args::{BaselineArgs, PositionalComponentArgs};
use super::{audit, lint, test, CmdResult, GlobalArgs};

mod render;

#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    /// Run audit + lint + test only against files changed since this git ref
    /// (branch, tag, or SHA). CI-friendly — mirrors the per-stage flag.
    #[arg(long, value_name = "REF", conflicts_with = "changed_only")]
    pub changed_since: Option<String>,

    /// Run only against files modified in the working tree
    /// (staged, unstaged, untracked). Only the lint stage scopes natively;
    /// audit and test run on the full component with a hint noting the
    /// limitation. Use `--changed-since` for full umbrella scoping.
    #[arg(long, conflicts_with = "changed_since")]
    pub changed_only: bool,

    /// Show compact summary instead of full per-stage output
    #[arg(long)]
    pub summary: bool,

    /// Hidden compatibility flag — the JSON envelope is always emitted at the
    /// CLI layer (`{success, data}`); this exists so callers that pass
    /// `--json` to other homeboy commands can pass it here too.
    #[arg(long, hide = true)]
    pub json: bool,

    /// Output format. Default JSON envelope; `--report=pr-comment` emits a
    /// markdown PR-comment section instead, suitable for piping to
    /// `homeboy git pr comment --body-file`.
    #[arg(long, value_name = "FORMAT", value_parser = ["pr-comment"])]
    pub report: Option<String>,

    /// Action-level banner rendered above the PR-comment scope line.
    /// Repeatable as `--banner key=value`.
    #[arg(long, value_name = "KEY=VALUE", value_parser = parse_key_val)]
    pub banner: Vec<(String, String)>,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,
}

/// True when the caller asked for a markdown PR-comment section instead of
/// the structured JSON envelope. Used by the top-level dispatcher to route
/// the response through `RawOutputMode::Markdown`.
pub fn is_markdown_mode(args: &ReviewArgs) -> bool {
    args.report.as_deref() == Some("pr-comment")
}

/// Per-stage section of the consolidated review output.
#[derive(Serialize)]
pub struct ReviewStage<T: Serialize> {
    /// Stage name (`"audit"`, `"lint"`, `"test"`).
    pub stage: String,
    /// Whether the stage ran or was skipped.
    pub ran: bool,
    /// Stage-level pass/fail (only meaningful when `ran` is true).
    pub passed: bool,
    /// Stage exit code (0 when skipped).
    pub exit_code: i32,
    /// Number of findings the stage reported (audit findings, lint findings,
    /// test failures). Always 0 when skipped or stage-internal counts unavailable.
    pub finding_count: usize,
    /// Human-readable hint pointing to the per-stage command for deep dive.
    pub hint: String,
    /// Skip reason (only present when `ran` is false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    /// Full structured output from the underlying command. None if skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<T>,
}

/// Top-level summary block — what a reviewer would skim first.
#[derive(Serialize)]
pub struct ReviewSummary {
    /// True when every stage that ran exited 0.
    pub passed: bool,
    /// Top-line status string.
    pub status: String,
    /// Component label.
    pub component: String,
    /// Scope mode applied: `"changed-since"`, `"changed-only"`, or `"full"`.
    pub scope: String,
    /// The git ref passed to `--changed-since`, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,
    /// Total findings across all stages that ran.
    pub total_findings: usize,
    /// Count of files in the changed set (None when not in scoped mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_file_count: Option<usize>,
    /// Top-level hints (e.g., empty changeset, scope warnings).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hints: Vec<String>,
}

/// Unified output envelope for the review command.
#[derive(Serialize)]
pub struct ReviewCommandOutput {
    pub command: String,
    pub artifact: ReviewArtifact,
    pub summary: ReviewSummary,
    pub audit: ReviewStage<AuditCommandOutput>,
    pub lint: ReviewStage<LintCommandOutput>,
    pub test: ReviewStage<TestCommandOutput>,
}

/// Stable machine-readable artifact for automated PR review consumers.
#[derive(Serialize, Clone)]
pub struct ReviewArtifact {
    pub schema: String,
    pub component: String,
    pub status: String,
    pub generated_at: String,
    pub base_ref: String,
    pub head_ref: String,
    pub commands: Vec<ReviewArtifactCommand>,
}

#[derive(Serialize, Clone)]
pub struct ReviewArtifactCommand {
    pub name: String,
    pub status: String,
    pub exit_code: i32,
    pub summary: String,
    pub findings: Vec<Value>,
    pub artifacts: Vec<Value>,
}

pub fn run(args: ReviewArgs, global: &GlobalArgs) -> CmdResult<ReviewCommandOutput> {
    // Resolve component ID (auto-discovers from CWD when omitted) and source
    // path so we can probe git for the changed-file set ourselves.
    let component = args.comp.load()?;
    let component_label = component.id.clone();
    let source_path = component.local_path.clone();

    let scope = if args.changed_since.is_some() {
        "changed-since"
    } else if args.changed_only {
        "changed-only"
    } else {
        "full"
    }
    .to_string();

    // Probe the changed set once at the umbrella level so we can short-circuit
    // before paying for any extension setup. Each stage will re-derive its
    // own scope internally (and that's fine — `get_files_changed_since` is
    // cheap, and lint/audit/test must remain independently invocable).
    let changed_file_count = match (&args.changed_since, args.changed_only) {
        (Some(git_ref), _) => Some(git::get_files_changed_since(&source_path, git_ref)?.len()),
        (_, true) => Some(git::get_dirty_files(&source_path)?.len()),
        _ => None,
    };

    if let Some(0) = changed_file_count {
        let scope_label = if let Some(ref r) = args.changed_since {
            format!("since {}", r)
        } else {
            "in working tree".to_string()
        };
        let message = format!("No files changed {} — skipping review", scope_label);
        println!("{}", message);

        let output = ReviewCommandOutput {
            command: "review".to_string(),
            artifact: ReviewArtifact {
                schema: "homeboy/review/v1".to_string(),
                component: component_label.clone(),
                status: "skipped".to_string(),
                generated_at: generated_at_now(),
                base_ref: args.changed_since.clone().unwrap_or_default(),
                head_ref: git_ref(&source_path, "HEAD").unwrap_or_default(),
                commands: vec![
                    artifact_command(&stage_skipped::<Value>("audit", "no files changed")),
                    artifact_command(&stage_skipped::<Value>("lint", "no files changed")),
                    artifact_command(&stage_skipped::<Value>("test", "no files changed")),
                ],
            },
            summary: ReviewSummary {
                passed: true,
                status: "passed".to_string(),
                component: component_label,
                scope,
                changed_since: args.changed_since.clone(),
                total_findings: 0,
                changed_file_count: Some(0),
                hints: vec![message],
            },
            audit: stage_skipped("audit", "no files changed"),
            lint: stage_skipped("lint", "no files changed"),
            test: stage_skipped("test", "no files changed"),
        };
        return Ok((output, 0));
    }

    // ── Stage 1: audit ──────────────────────────────────────────────────────
    // Audit scopes via --changed-since only (no --changed-only support today).
    // When the user asked for --changed-only, audit runs against the full
    // component; we surface that limitation in `summary.hints`.
    let mut top_hints: Vec<String> = Vec::new();

    let audit_args = audit::AuditArgs {
        comp: args.comp.clone(),
        conventions: false,
        only: Vec::new(),
        exclude: Vec::new(),
        baseline_args: args.baseline_args.clone(),
        changed_since: args.changed_since.clone(),
        json_summary: args.summary,
        fixability: false,
    };
    let (audit_output, audit_exit) = audit::run(audit_args, global)?;
    let audit_passed = audit_exit == 0;
    let audit_findings = audit_finding_count(&audit_output);
    let audit_stage = ReviewStage {
        stage: "audit".to_string(),
        ran: true,
        passed: audit_passed,
        exit_code: audit_exit,
        finding_count: audit_findings,
        hint: format!(
            "Deep dive: homeboy audit {}{}",
            component_label,
            scope_flag_suffix(&args, /*include_changed_only=*/ false),
        ),
        skipped_reason: None,
        output: Some(audit_output),
    };

    // ── Stage 2: lint ───────────────────────────────────────────────────────
    let lint_args = lint::LintArgs {
        comp: args.comp.clone(),
        summary: args.summary,
        file: None,
        glob: None,
        changed_only: args.changed_only,
        changed_since: args.changed_since.clone(),
        errors_only: false,
        sniffs: None,
        exclude_sniffs: None,
        category: None,
        fix: false,
        setting_args: Default::default(),
        baseline_args: args.baseline_args.clone(),
        _json: Default::default(),
    };
    let (lint_output, lint_exit) = lint::run(lint_args, global)?;
    let lint_passed = lint_exit == 0;
    let lint_findings = lint_finding_count(&lint_output);
    let lint_stage = ReviewStage {
        stage: "lint".to_string(),
        ran: true,
        passed: lint_passed,
        exit_code: lint_exit,
        finding_count: lint_findings,
        hint: format!(
            "Deep dive: homeboy lint {}{}",
            component_label,
            scope_flag_suffix(&args, /*include_changed_only=*/ true),
        ),
        skipped_reason: None,
        output: Some(lint_output),
    };

    // ── Stage 3: test ───────────────────────────────────────────────────────
    // Test scopes via --changed-since only (same as audit). When the user
    // passed --changed-only, test runs the full suite — surface as a hint.
    let test_args = test::TestArgs {
        comp: args.comp.clone(),
        skip_lint: true, // lint already ran above; avoid double work
        coverage: false,
        coverage_min: None,
        baseline_args: args.baseline_args.clone(),
        analyze: false,
        drift: false,
        write: false,
        since: "HEAD~10".to_string(),
        changed_since: args.changed_since.clone(),
        setting_args: Default::default(),
        args: Vec::new(),
        _json: Default::default(),
        json_summary: args.summary,
    };
    let (test_output, test_exit) = test::run(test_args, global)?;
    let test_passed = test_exit == 0;
    let test_findings = test_finding_count(&test_output);
    let test_stage = ReviewStage {
        stage: "test".to_string(),
        ran: true,
        passed: test_passed,
        exit_code: test_exit,
        finding_count: test_findings,
        hint: format!(
            "Deep dive: homeboy test {}{}",
            component_label,
            scope_flag_suffix(&args, /*include_changed_only=*/ false),
        ),
        skipped_reason: None,
        output: Some(test_output),
    };

    // Aggregate
    let overall_passed = audit_passed && lint_passed && test_passed;
    let overall_exit = if overall_passed {
        0
    } else if [audit_exit, lint_exit, test_exit].iter().any(|&c| c >= 2) {
        2
    } else {
        1
    };
    let total_findings = audit_findings + lint_findings + test_findings;

    if args.changed_only {
        top_hints.push(
            "--changed-only scopes lint only; audit and test ran on the full component".to_string(),
        );
    }

    let summary = ReviewSummary {
        passed: overall_passed,
        status: if overall_passed { "passed" } else { "failed" }.to_string(),
        component: component_label.clone(),
        scope,
        changed_since: args.changed_since.clone(),
        total_findings,
        changed_file_count,
        hints: top_hints,
    };

    let artifact = build_artifact(
        &component_label,
        args.changed_since.as_deref().unwrap_or(""),
        git_ref(&source_path, "HEAD").unwrap_or_default().as_str(),
        vec![
            artifact_command(&audit_stage),
            artifact_command(&lint_stage),
            artifact_command(&test_stage),
        ],
    );

    let output = ReviewCommandOutput {
        command: "review".to_string(),
        artifact,
        summary,
        audit: audit_stage,
        lint: lint_stage,
        test: test_stage,
    };

    print_human_summary(&output);

    Ok((output, overall_exit))
}

/// Markdown output mode — runs the JSON path internally and renders the
/// envelope into a PR-comment section. The body is just the section content;
/// the consumer (`homeboy git pr comment --header`) owns the wrapping
/// section header.
pub fn run_markdown(args: ReviewArgs, global: &GlobalArgs) -> CmdResult<String> {
    let banners = args.banner.clone();
    let (output, exit_code) = run(args, global)?;
    let md = if banners.is_empty() {
        render::render_pr_comment(&output)
    } else {
        render::render_pr_comment_with_banners(&output, &banners)
    };
    Ok((md, exit_code))
}

/// Write the stable review artifact to `--output` for automated consumers.
/// Falls back to the generic JSON envelope if the review command failed before
/// producing an artifact.
pub fn write_artifact_to_file(
    result: &homeboy::Result<Value>,
    path: &str,
    _exit_code: i32,
) -> bool {
    let Ok(data) = result else {
        return false;
    };
    let Some(artifact) = data.get("artifact") else {
        return false;
    };

    let json = match serde_json::to_string_pretty(artifact) {
        Ok(j) => j,
        Err(e) => {
            eprintln!(
                "Warning: failed to serialize review artifact for --output: {}",
                e
            );
            return true;
        }
    };

    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "Warning: failed to create --output directory '{}': {}",
                    parent.display(),
                    e
                );
                return true;
            }
        }
    }

    if let Err(e) = std::fs::write(path, json) {
        eprintln!("Warning: failed to write --output file '{}': {}", path, e);
    }
    true
}

fn stage_skipped<T: Serialize>(stage: &str, reason: &str) -> ReviewStage<T> {
    ReviewStage {
        stage: stage.to_string(),
        ran: false,
        passed: true,
        exit_code: 0,
        finding_count: 0,
        hint: format!("Run individually: homeboy {}", stage),
        skipped_reason: Some(reason.to_string()),
        output: None,
    }
}

fn scope_flag_suffix(args: &ReviewArgs, include_changed_only: bool) -> String {
    if let Some(ref r) = args.changed_since {
        format!(" --changed-since={}", r)
    } else if args.changed_only && include_changed_only {
        " --changed-only".to_string()
    } else {
        String::new()
    }
}

fn build_artifact(
    component: &str,
    base_ref: &str,
    head_ref: &str,
    commands: Vec<ReviewArtifactCommand>,
) -> ReviewArtifact {
    let status = artifact_status(&commands).to_string();
    ReviewArtifact {
        schema: "homeboy/review/v1".to_string(),
        component: component.to_string(),
        status,
        generated_at: generated_at_now(),
        base_ref: base_ref.to_string(),
        head_ref: head_ref.to_string(),
        commands,
    }
}

fn artifact_command<T: Serialize>(stage: &ReviewStage<T>) -> ReviewArtifactCommand {
    ReviewArtifactCommand {
        name: stage.stage.clone(),
        status: if !stage.ran {
            "skipped"
        } else if stage.passed {
            "passed"
        } else {
            "failed"
        }
        .to_string(),
        exit_code: stage.exit_code,
        summary: if !stage.ran {
            stage
                .skipped_reason
                .clone()
                .unwrap_or_else(|| "skipped".to_string())
        } else {
            format!(
                "{} finding(s); {}",
                stage.finding_count,
                if stage.passed { "passed" } else { "failed" }
            )
        },
        findings: Vec::new(),
        artifacts: Vec::new(),
    }
}

fn artifact_status(commands: &[ReviewArtifactCommand]) -> &'static str {
    let ran = commands
        .iter()
        .filter(|command| command.status != "skipped")
        .count();
    if ran == 0 {
        return "skipped";
    }
    if commands.iter().any(|command| command.status == "failed") {
        return "failed";
    }
    if ran < commands.len() {
        return "partial";
    }
    "passed"
}

fn generated_at_now() -> String {
    Utc::now().to_rfc3339()
}

fn git_ref(path: &str, git_ref: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", git_ref])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn audit_finding_count(output: &AuditCommandOutput) -> usize {
    match output {
        AuditCommandOutput::Full { result, .. } => result.findings.len(),
        AuditCommandOutput::Compared { result, .. } => result.findings.len(),
        AuditCommandOutput::Summary(summary) => summary.total_findings,
        AuditCommandOutput::BaselineSaved { findings_count, .. } => *findings_count,
        AuditCommandOutput::Conventions { .. } => 0,
    }
}

fn lint_finding_count(output: &LintCommandOutput) -> usize {
    output.lint_findings.as_ref().map(|f| f.len()).unwrap_or(0)
}

fn test_finding_count(output: &TestCommandOutput) -> usize {
    output
        .test_counts
        .as_ref()
        .map(|c| c.failed as usize)
        .unwrap_or(0)
}

/// Print a compact human-readable summary to stderr so users running
/// `homeboy review` interactively see a skim-friendly report on top of the
/// JSON envelope. Mirrors the per-command stderr status hints.
fn print_human_summary(output: &ReviewCommandOutput) {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return;
    }

    eprintln!();
    eprintln!(
        "[review] {}: {} (component {}, scope {})",
        if output.summary.passed {
            "PASS"
        } else {
            "FAIL"
        },
        output.summary.status,
        output.summary.component,
        output.summary.scope,
    );
    print_stage_line(&output.audit);
    print_stage_line(&output.lint);
    print_stage_line(&output.test);
    for hint in &output.summary.hints {
        eprintln!("[review] hint: {}", hint);
    }
}

fn print_stage_line<T: Serialize>(stage: &ReviewStage<T>) {
    let marker = if !stage.ran {
        "skipped"
    } else if stage.passed {
        "passed"
    } else {
        "failed"
    };
    eprintln!(
        "[review]   {:<6} {:<7} findings={} exit={}",
        stage.stage, marker, stage.finding_count, stage.exit_code,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{BaselineArgs, PositionalComponentArgs};
    use clap::Parser;

    /// Minimal CLI wrapper to exercise clap parsing of `ReviewArgs`.
    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        review: ReviewArgs,
    }

    #[test]
    fn parses_changed_since() {
        let cli = TestCli::try_parse_from(["test", "my-comp", "--changed-since", "trunk"])
            .expect("should parse");
        assert_eq!(cli.review.changed_since.as_deref(), Some("trunk"));
        assert!(!cli.review.changed_only);
        assert_eq!(cli.review.comp.component.as_deref(), Some("my-comp"));
    }

    #[test]
    fn parses_changed_only() {
        let cli = TestCli::try_parse_from(["test", "--changed-only"]).expect("should parse");
        assert!(cli.review.changed_only);
        assert!(cli.review.changed_since.is_none());
    }

    #[test]
    fn parses_report_pr_comment() {
        let cli = TestCli::try_parse_from(["test", "my-comp", "--report=pr-comment"])
            .expect("should parse");
        assert_eq!(cli.review.report.as_deref(), Some("pr-comment"));
        assert!(is_markdown_mode(&cli.review));
    }

    #[test]
    fn parses_repeatable_pr_comment_banners_in_order() {
        let cli = TestCli::try_parse_from([
            "test",
            "my-comp",
            "--report=pr-comment",
            "--banner",
            "autofix=applied 3 file(s)",
            "--banner=binary-source=fallback",
            "--banner",
            "custom=value=with=equals",
        ])
        .expect("should parse repeatable banners");

        assert_eq!(
            cli.review.banner,
            vec![
                ("autofix".to_string(), "applied 3 file(s)".to_string()),
                ("binary-source".to_string(), "fallback".to_string()),
                ("custom".to_string(), "value=with=equals".to_string()),
            ]
        );
    }

    #[test]
    fn rejects_unknown_report_format() {
        let result = TestCli::try_parse_from(["test", "my-comp", "--report=slack"]);
        assert!(
            result.is_err(),
            "clap whitelist must reject unknown report formats"
        );
    }

    #[test]
    fn is_markdown_mode_false_without_flag() {
        let cli = TestCli::try_parse_from(["test", "my-comp"]).expect("should parse");
        assert!(!is_markdown_mode(&cli.review));
    }

    #[test]
    fn parses_with_no_component() {
        let cli = TestCli::try_parse_from(["test", "--changed-since", "main"])
            .expect("should parse without positional component");
        assert!(cli.review.comp.component.is_none());
    }

    #[test]
    fn rejects_changed_since_with_changed_only() {
        let result =
            TestCli::try_parse_from(["test", "--changed-since", "trunk", "--changed-only"]);
        assert!(result.is_err(), "clap must reject conflicting scope flags");
    }

    #[test]
    fn rejects_changed_only_with_changed_since() {
        let result =
            TestCli::try_parse_from(["test", "--changed-only", "--changed-since", "trunk"]);
        assert!(result.is_err());
    }

    #[test]
    fn parses_summary_and_baseline_flags() {
        let cli = TestCli::try_parse_from([
            "test",
            "my-comp",
            "--changed-since=trunk",
            "--summary",
            "--ignore-baseline",
        ])
        .expect("should parse");
        assert!(cli.review.summary);
        assert!(cli.review.baseline_args.ignore_baseline);
    }

    #[test]
    fn stage_skipped_helper_marks_not_ran() {
        let stage: ReviewStage<serde_json::Value> = stage_skipped("audit", "no files changed");
        assert!(!stage.ran);
        assert!(stage.passed);
        assert_eq!(stage.exit_code, 0);
        assert_eq!(stage.skipped_reason.as_deref(), Some("no files changed"));
    }

    #[test]
    fn artifact_command_maps_stage_statuses() {
        let skipped: ReviewStage<serde_json::Value> = stage_skipped("audit", "no files changed");
        let skipped_command = artifact_command(&skipped);
        assert_eq!(skipped_command.name, "audit");
        assert_eq!(skipped_command.status, "skipped");
        assert_eq!(skipped_command.exit_code, 0);
        assert_eq!(skipped_command.summary, "no files changed");
        assert!(skipped_command.findings.is_empty());
        assert!(skipped_command.artifacts.is_empty());

        let failed = ReviewStage {
            stage: "lint".to_string(),
            ran: true,
            passed: false,
            exit_code: 1,
            finding_count: 3,
            hint: "Deep dive: homeboy lint".to_string(),
            skipped_reason: None,
            output: Some(serde_json::json!({ "ok": false })),
        };
        let failed_command = artifact_command(&failed);
        assert_eq!(failed_command.name, "lint");
        assert_eq!(failed_command.status, "failed");
        assert_eq!(failed_command.exit_code, 1);
        assert_eq!(failed_command.summary, "3 finding(s); failed");
    }

    #[test]
    fn artifact_status_covers_contract_values() {
        let passed = ReviewArtifactCommand {
            name: "lint".to_string(),
            status: "passed".to_string(),
            exit_code: 0,
            summary: "0 finding(s); passed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };
        let skipped = ReviewArtifactCommand {
            name: "test".to_string(),
            status: "skipped".to_string(),
            exit_code: 0,
            summary: "no files changed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };
        let failed = ReviewArtifactCommand {
            name: "audit".to_string(),
            status: "failed".to_string(),
            exit_code: 1,
            summary: "1 finding(s); failed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };

        assert_eq!(artifact_status(std::slice::from_ref(&skipped)), "skipped");
        assert_eq!(artifact_status(std::slice::from_ref(&passed)), "passed");
        assert_eq!(artifact_status(&[passed.clone(), skipped]), "partial");
        assert_eq!(artifact_status(&[passed, failed]), "failed");
    }

    #[test]
    fn build_artifact_uses_review_schema_and_refs() {
        let command = ReviewArtifactCommand {
            name: "lint".to_string(),
            status: "passed".to_string(),
            exit_code: 0,
            summary: "0 finding(s); passed".to_string(),
            findings: Vec::new(),
            artifacts: Vec::new(),
        };

        let artifact = build_artifact("homeboy", "origin/main", "abc123", vec![command]);

        assert_eq!(artifact.schema, "homeboy/review/v1");
        assert_eq!(artifact.component, "homeboy");
        assert_eq!(artifact.status, "passed");
        assert_eq!(artifact.base_ref, "origin/main");
        assert_eq!(artifact.head_ref, "abc123");
        assert_eq!(artifact.commands.len(), 1);
        assert!(artifact.generated_at.contains('T'));
    }

    #[test]
    fn write_artifact_to_file_writes_direct_artifact_and_creates_parent_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("homeboy-ci-results").join("review.json");
        let result = Ok(serde_json::json!({
            "command": "review",
            "artifact": {
                "schema": "homeboy/review/v1",
                "component": "homeboy",
                "status": "passed",
                "generated_at": "2026-04-28T00:00:00Z",
                "base_ref": "origin/main",
                "head_ref": "abc123",
                "commands": []
            }
        }));

        assert!(write_artifact_to_file(
            &result,
            path.to_str().expect("utf8 path"),
            0
        ));

        let written = std::fs::read_to_string(path).expect("artifact written");
        let json: serde_json::Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["schema"], "homeboy/review/v1");
        assert!(
            json.get("success").is_none(),
            "artifact is not CLI envelope"
        );
    }

    #[test]
    fn scope_flag_suffix_renders_changed_since() {
        let args = ReviewArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            changed_since: Some("trunk".to_string()),
            changed_only: false,
            summary: false,
            json: false,
            report: None,
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        };
        assert_eq!(scope_flag_suffix(&args, true), " --changed-since=trunk");
        assert_eq!(scope_flag_suffix(&args, false), " --changed-since=trunk");
    }

    #[test]
    fn scope_flag_suffix_renders_changed_only_only_when_allowed() {
        let args = ReviewArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            changed_since: None,
            changed_only: true,
            summary: false,
            json: false,
            report: None,
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        };
        assert_eq!(scope_flag_suffix(&args, true), " --changed-only");
        // audit/test do not support --changed-only, so the suffix is empty
        // when the caller requests it not be included.
        assert_eq!(scope_flag_suffix(&args, false), "");
    }

    #[test]
    fn scope_flag_suffix_empty_for_full_run() {
        let args = ReviewArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            changed_since: None,
            changed_only: false,
            summary: false,
            json: false,
            report: None,
            banner: Vec::new(),
            baseline_args: BaselineArgs::default(),
        };
        assert_eq!(scope_flag_suffix(&args, true), "");
        assert_eq!(scope_flag_suffix(&args, false), "");
    }
}
