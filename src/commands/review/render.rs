//! PR-comment markdown renderer for `homeboy review`.
//!
//! Pure function over `ReviewCommandOutput` — no I/O, no template engine.
//! The renderer emits the *body* of a PR comment section; the consumer
//! (`homeboy git pr comment --header`) owns the wrapping `### Title` heading.
//!
//! Shape (top-down):
//!
//! 1. Optional caller-supplied banners for action-level signals.
//! 2. Scope banner: `:zap:` for changed-since/changed-only, `:information_source:` for full.
//! 3. Total findings line.
//! 4. Stage blocks in fixed order — audit, lint, test, optional ci. Each stage:
//!    - icon + name header (`:white_check_mark:`, `:x:`, `:fast_forward:`)
//!    - up to 10 finding bullets (top categories / sniff codes / failure summary)
//!    - blockquote with the deep-dive hint
//!

use std::fmt::Write as _;

use homeboy::core::ci_profile::CiRunOutput;
use homeboy::core::code_audit::report::AuditSummaryFinding;
use homeboy::core::code_audit::{AuditCommandOutput, AuditFinding, Finding, Severity};
use homeboy::core::extension::lint::LintCommandOutput;
use homeboy::core::extension::test::TestCommandOutput;
use homeboy::core::finding::HomeboyFinding;
use homeboy::core::top_n::top_n_by;

use super::{ReviewCommandOutput, ReviewStage};

/// Maximum bullets shown per stage. Anything beyond is collapsed into a
/// `(... N more)` hint so PR comments stay skim-friendly.
const TOP_N: usize = 10;

/// Render a `ReviewCommandOutput` into a PR-comment-ready markdown body.
pub fn render_pr_comment(output: &ReviewCommandOutput) -> String {
    render_pr_comment_with_banners(output, &[])
}

/// Render a `ReviewCommandOutput` with optional action-level banner lines.
pub fn render_pr_comment_with_banners(
    output: &ReviewCommandOutput,
    banners: &[(String, String)],
) -> String {
    let mut out = String::new();

    render_banners(&mut out, banners);
    render_scope_banner(&mut out, output);
    render_total_findings(&mut out, output);
    render_top_hints(&mut out, output);

    render_audit_stage(&mut out, &output.audit);
    out.push('\n');
    render_lint_stage(&mut out, &output.lint);
    out.push('\n');
    render_test_stage(&mut out, &output.test);
    if let Some(ref stage) = output.ci_profile {
        out.push('\n');
        render_ci_stage(&mut out, stage);
    }

    out
}

// ── Top-level banners ───────────────────────────────────────────────────

fn render_banners(out: &mut String, banners: &[(String, String)]) {
    if banners.is_empty() {
        return;
    }

    for (key, value) in banners {
        let _ = writeln!(out, "> {} **{}:** {}", banner_icon(key), key, value);
    }
    out.push('\n');
}

fn banner_icon(key: &str) -> &'static str {
    match key {
        "autofix" => ":wrench:",
        "binary-source" => ":warning:",
        "scope-mode" => ":information_source:",
        _ => ":information_source:",
    }
}

fn render_scope_banner(out: &mut String, output: &ReviewCommandOutput) {
    match output.summary.scope.as_str() {
        "changed-since" => {
            let r = output.summary.changed_since.as_deref().unwrap_or("<base>");
            let _ = writeln!(out, ":zap: Scope: **changed files only** (since `{}`)", r);
        }
        "changed-only" => {
            let _ = writeln!(out, ":zap: Scope: **working tree changes only**");
        }
        _ => {
            let _ = writeln!(out, ":information_source: Scope: **full**");
        }
    }
    out.push('\n');
}

fn render_total_findings(out: &mut String, output: &ReviewCommandOutput) {
    let ran = [output.audit.ran, output.lint.ran, output.test.ran]
        .into_iter()
        .filter(|ran| *ran)
        .count()
        + output
            .ci_profile
            .as_ref()
            .map(|stage| usize::from(stage.ran))
            .unwrap_or(0);
    let _ = writeln!(
        out,
        "**{}** finding(s) across {} stage(s)",
        output.summary.total_findings, ran
    );
    out.push('\n');
}

fn render_top_hints(out: &mut String, output: &ReviewCommandOutput) {
    if output.summary.hints.is_empty() {
        return;
    }
    for hint in &output.summary.hints {
        let _ = writeln!(out, "> :information_source: {}", hint);
    }
    out.push('\n');
}

// ── Stage rendering ─────────────────────────────────────────────────────

fn stage_header_icon(stage_ran: bool, stage_passed: bool) -> &'static str {
    if !stage_ran {
        ":fast_forward:"
    } else if stage_passed {
        ":white_check_mark:"
    } else {
        ":x:"
    }
}

fn render_stage_header<T: serde::Serialize>(out: &mut String, stage: &ReviewStage<T>) {
    let icon = stage_header_icon(stage.ran, stage.passed);
    if !stage.ran {
        let reason = stage.skipped_reason.as_deref().unwrap_or("not run");
        let _ = writeln!(out, "{} **{}** — skipped ({})", icon, stage.stage, reason);
    } else {
        let _ = writeln!(out, "{} **{}**", icon, stage.stage);
    }
}

fn render_stage_hint<T: serde::Serialize>(out: &mut String, stage: &ReviewStage<T>) {
    if stage.ran {
        let _ = writeln!(out, "> {}", stage.hint);
    }
}

fn render_audit_stage(out: &mut String, stage: &ReviewStage<AuditCommandOutput>) {
    render_stage_header(out, stage);
    if let Some(ref output) = stage.output {
        render_audit_body(out, output);
    }
    render_stage_hint(out, stage);
}

fn render_lint_stage(out: &mut String, stage: &ReviewStage<LintCommandOutput>) {
    render_stage_header(out, stage);
    if let Some(ref output) = stage.output {
        render_lint_body(out, output);
    }
    render_stage_hint(out, stage);
}

fn render_test_stage(out: &mut String, stage: &ReviewStage<TestCommandOutput>) {
    render_stage_header(out, stage);
    if let Some(ref output) = stage.output {
        render_test_body(out, output);
    }
    render_stage_hint(out, stage);
}

fn render_ci_stage(out: &mut String, stage: &ReviewStage<CiRunOutput>) {
    render_stage_header(out, stage);
    if let Some(ref output) = stage.output {
        render_ci_body(out, output);
    }
    render_stage_hint(out, stage);
}

// ── Per-stage bodies ────────────────────────────────────────────────────

/// Render the audit stage body as actionable finding snippets. Empty bodies
/// mean no findings; we say nothing.
fn render_audit_body(out: &mut String, output: &AuditCommandOutput) {
    let findings = audit_findings(output);
    if findings.is_empty() {
        return;
    }

    for finding in findings.iter().take(TOP_N) {
        render_audit_finding(out, finding);
    }
    if findings.len() > TOP_N {
        let _ = writeln!(
            out,
            "- _… {} more audit finding(s)_",
            findings.len() - TOP_N
        );
    }
    let _ = writeln!(out, "- _Total: {} finding(s)_", findings.len());
}

fn render_audit_finding(out: &mut String, finding: &AuditFindingLine) {
    let _ = write!(
        out,
        "- `{}` — **{}**: {}",
        finding.file,
        audit_kind_label(&finding.kind),
        finding.description
    );
    if !finding.suggestion.is_empty() {
        let _ = write!(out, "; {}", finding.suggestion);
    }
    if matches!(finding.severity, Severity::Info) {
        let _ = write!(out, " _(info)_");
    }
    out.push('\n');
}

#[derive(Clone)]
struct AuditFindingLine {
    file: String,
    kind: AuditFinding,
    severity: Severity,
    description: String,
    suggestion: String,
}

impl From<&Finding> for AuditFindingLine {
    fn from(finding: &Finding) -> Self {
        Self {
            file: finding.file.clone(),
            kind: finding.kind.clone(),
            severity: finding.severity.clone(),
            description: finding.description.clone(),
            suggestion: finding.suggestion.clone(),
        }
    }
}

impl From<&AuditSummaryFinding> for AuditFindingLine {
    fn from(finding: &AuditSummaryFinding) -> Self {
        Self {
            file: finding.file.clone(),
            kind: finding.kind.clone(),
            severity: finding.severity.clone(),
            description: finding.description.clone(),
            suggestion: finding.suggestion.clone(),
        }
    }
}

/// Pull actionable audit finding details. PR comments should answer "where and
/// why?" without forcing the reviewer to run the deep-dive command first.
fn audit_findings(output: &AuditCommandOutput) -> Vec<AuditFindingLine> {
    match output {
        AuditCommandOutput::Full { result, .. } => {
            result.findings.iter().map(AuditFindingLine::from).collect()
        }
        AuditCommandOutput::Compared { result, .. } => {
            result.findings.iter().map(AuditFindingLine::from).collect()
        }
        AuditCommandOutput::Summary(summary) => summary
            .top_findings
            .iter()
            .map(AuditFindingLine::from)
            .collect(),
        AuditCommandOutput::BaselineSaved { .. } => Vec::new(),
        AuditCommandOutput::Conventions { .. } => Vec::new(),
    }
}

fn audit_kind_label(kind: &AuditFinding) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{:?}", kind).to_lowercase())
}

/// Render the lint stage body — top sniff codes (by `category`) with counts.
fn render_lint_body(out: &mut String, output: &LintCommandOutput) {
    let findings = match output.findings.as_ref() {
        Some(f) if !f.is_empty() => f,
        _ => return,
    };

    let buckets = top_n_by(
        findings,
        |f| f.category.clone().unwrap_or_else(|| f.tool.clone()),
        TOP_N,
    );
    for (code, count) in &buckets.items {
        let _ = writeln!(out, "- `{}` — {} finding(s)", code, count);
    }
    if buckets.remainder > 0 {
        let _ = writeln!(out, "- _… {} more sniff(s)_", buckets.remainder);
    }
    let _ = writeln!(out, "- _Total: {} finding(s)_", buckets.total);
}

/// Render the test stage body — structured failure names when present, with
/// the aggregate-only fallback preserved for runners that only report counts.
fn render_test_body(out: &mut String, output: &TestCommandOutput) {
    let counts = match output.test_counts.as_ref() {
        Some(c) => c,
        None => return,
    };

    if counts.failed > 0 {
        let _ = writeln!(
            out,
            "- **{} failed** out of {} total",
            counts.failed, counts.total
        );
        if let Some(findings) = output.findings.as_ref() {
            render_test_findings(out, findings);
        }
        if counts.passed > 0 {
            let _ = writeln!(out, "- {} passed", counts.passed);
        }
        if counts.skipped > 0 {
            let _ = writeln!(out, "- {} skipped", counts.skipped);
        }
    } else if counts.passed > 0 {
        let _ = writeln!(out, "- {} passed", counts.passed);
        if counts.skipped > 0 {
            let _ = writeln!(out, "- {} skipped", counts.skipped);
        }
    }
}

fn render_test_findings(out: &mut String, findings: &[HomeboyFinding]) {
    for finding in findings.iter().take(TOP_N) {
        let name = finding
            .metadata
            .get("test_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&finding.message);
        let _ = write!(out, "- `{}`", name);
        if name != finding.message {
            let _ = write!(out, " — {}", finding.message);
        }
        if let Some(file) = finding.location.file.as_deref() {
            let _ = write!(out, " (`{}", file);
            if let Some(line) = finding.location.line {
                let _ = write!(out, ":{}", line);
            }
            let _ = write!(out, "`)");
        }
        out.push('\n');
    }

    if findings.len() > TOP_N {
        let _ = writeln!(out, "- _… {} more failed test(s)_", findings.len() - TOP_N);
    }
}

fn render_ci_body(out: &mut String, output: &CiRunOutput) {
    for job in &output.jobs {
        let marker = if job.success { "passed" } else { "failed" };
        let _ = writeln!(out, "- `{}` — {} (exit {})", job.id, marker, job.exit_code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use homeboy::core::ci_profile::{CiContext, CiJobRunOutput, CiRunOutput, CiRunSelection};
    use homeboy::core::code_audit::{
        AuditCommandOutput, AuditFinding, CodeAuditResult, Finding, Severity,
    };
    use homeboy::core::extension::lint::LintCommandOutput;
    use homeboy::core::extension::test::{TestCommandOutput, TestCounts};
    use homeboy::core::extension::CiJobMapping;
    use homeboy::core::extension::{PhaseReport, PhaseStatus, VerificationPhase};
    use homeboy::core::finding::HomeboyFinding;
    use homeboy::core::quality::{build_quality_plan, QualityPlanOptions};

    // ── Builders for fixture envelopes ──────────────────────────────────

    fn passing_envelope() -> ReviewCommandOutput {
        ReviewCommandOutput {
            command: "review".to_string(),
            plan: build_quality_plan(QualityPlanOptions::review("my-comp")),
            observation: None,
            artifact: super::super::build_artifact("my-comp", "", "abc123", Vec::new()),
            summary: super::super::ReviewSummary {
                passed: true,
                status: "passed".to_string(),
                component: "my-comp".to_string(),
                scope: "full".to_string(),
                changed_since: None,
                total_findings: 0,
                changed_file_count: None,
                hints: Vec::new(),
            },
            audit: stage_audit_passing(),
            lint: stage_lint_passing(),
            test: stage_test_passing(0),
            ci_profile: None,
        }
    }

    fn stage_audit_passing() -> ReviewStage<AuditCommandOutput> {
        ReviewStage {
            stage: "audit".to_string(),
            ran: true,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: "Deep dive: homeboy audit my-comp".to_string(),
            skipped_reason: None,
            output: Some(audit_full_with_findings(Vec::new())),
        }
    }

    fn stage_lint_passing() -> ReviewStage<LintCommandOutput> {
        ReviewStage {
            stage: "lint".to_string(),
            ran: true,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: "Deep dive: homeboy lint my-comp".to_string(),
            skipped_reason: None,
            output: Some(lint_with_findings(Vec::new())),
        }
    }

    fn stage_test_passing(passed: u64) -> ReviewStage<TestCommandOutput> {
        ReviewStage {
            stage: "test".to_string(),
            ran: true,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: "Deep dive: homeboy test my-comp".to_string(),
            skipped_reason: None,
            output: Some(test_with_counts(passed, 0, 0)),
        }
    }

    fn stage_ci_with_jobs(jobs: Vec<CiJobRunOutput>) -> ReviewStage<CiRunOutput> {
        let passed = jobs.iter().all(|job| job.success);
        ReviewStage {
            stage: "ci".to_string(),
            ran: true,
            passed,
            exit_code: if passed { 0 } else { 1 },
            finding_count: jobs.iter().filter(|job| !job.success).count(),
            hint: "Deep dive: homeboy ci run my-comp --profile pr".to_string(),
            skipped_reason: None,
            output: Some(CiRunOutput {
                extension_id: "test".to_string(),
                selection: CiRunSelection::Profile("pr".to_string()),
                jobs,
                success: passed,
                exit_code: if passed { 0 } else { 1 },
            }),
        }
    }

    fn ci_job(id: &str, success: bool, exit_code: i32) -> CiJobRunOutput {
        CiJobRunOutput {
            id: id.to_string(),
            ci_context: CiContext {
                profile: Some("pr".to_string()),
                job_id: id.to_string(),
                mapping: CiJobMapping::default(),
                local_context: Default::default(),
            },
            command: "sh".to_string(),
            args: Vec::new(),
            success,
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn stage_skipped<T: serde::Serialize>(name: &str, reason: &str) -> ReviewStage<T> {
        ReviewStage {
            stage: name.to_string(),
            ran: false,
            passed: true,
            exit_code: 0,
            finding_count: 0,
            hint: format!("Run individually: homeboy {}", name),
            skipped_reason: Some(reason.to_string()),
            output: None,
        }
    }

    fn audit_full_with_findings(conventions: Vec<&str>) -> AuditCommandOutput {
        let findings: Vec<Finding> = conventions
            .into_iter()
            .map(|c| Finding {
                convention: c.to_string(),
                severity: Severity::Warning,
                file: "src/foo.rs".to_string(),
                description: "deviates from convention".to_string(),
                suggestion: "align with siblings".to_string(),
                kind: AuditFinding::MissingMethod,
            })
            .collect();

        let result = CodeAuditResult {
            component_id: "my-comp".to_string(),
            source_path: "/tmp/my-comp".to_string(),
            summary: homeboy::core::code_audit::AuditSummary {
                files_scanned: 0,
                conventions_detected: 0,
                outliers_found: 0,
                alignment_score: Some(1.0),
                files_skipped: 0,
                warnings: Vec::new(),
            },
            conventions: Vec::new(),
            directory_conventions: Vec::new(),
            findings,
            duplicate_groups: Vec::new(),
        };

        AuditCommandOutput::Full {
            passed: result.findings.is_empty(),
            result,
            fixability: None,
        }
    }

    fn lint_with_findings(items: Vec<(&str, &str)>) -> LintCommandOutput {
        let findings: Vec<HomeboyFinding> = items
            .into_iter()
            .enumerate()
            .map(|(idx, (category, msg))| {
                HomeboyFinding::builder("lint", msg)
                    .category(category)
                    .fingerprint(format!("lint-{}", idx))
                    .build()
            })
            .collect();

        let exit_code = if findings.is_empty() { 0 } else { 1 };

        LintCommandOutput {
            passed: exit_code == 0,
            status: if exit_code == 0 { "passed" } else { "failed" }.to_string(),
            component: "my-comp".to_string(),
            exit_code,
            phase: PhaseReport {
                phase: VerificationPhase::Lint,
                status: if exit_code == 0 {
                    PhaseStatus::Passed
                } else {
                    PhaseStatus::Failed
                },
                exit_code: Some(exit_code),
                summary: "lint phase".to_string(),
            },
            failure: None,
            autofix: None,
            hints: None,
            baseline_comparison: None,
            findings: Some(findings),
            producer_summaries: Vec::new(),
            summary: None,
            self_check_capture: None,
            ci_context: None,
        }
    }

    fn test_with_counts(passed: u64, failed: u64, skipped: u64) -> TestCommandOutput {
        let total = passed + failed + skipped;
        let exit_code: i32 = if failed == 0 { 0 } else { 1 };
        TestCommandOutput {
            passed: exit_code == 0,
            status: if exit_code == 0 { "passed" } else { "failed" }.to_string(),
            component: "my-comp".to_string(),
            exit_code,
            phase: None,
            failure: None,
            test_counts: Some(TestCounts {
                total,
                passed,
                failed,
                skipped,
            }),
            findings: None,
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            drift: None,
            auto_fix_drift: None,
            test_scope: None,
            summary: None,
            raw_output: None,
            ci_context: None,
        }
    }

    fn test_finding(idx: usize) -> HomeboyFinding {
        HomeboyFinding::builder("test", format!("assertion {} failed", idx))
            .severity("error")
            .file("tests/suite.rs")
            .line((idx + 10) as i64)
            .metadata("test_name", format!("tests::suite::case_{:02}", idx))
            .build()
    }

    fn test_with_findings(total: usize) -> TestCommandOutput {
        let mut output = test_with_counts(20, total as u64, 1);
        output.findings = Some((0..total).map(test_finding).collect());
        output
    }

    // ── Tests ───────────────────────────────────────────────────────────

    #[test]
    fn renders_passing_review_with_full_scope() {
        let env = passing_envelope();
        let md = render_pr_comment(&env);
        assert!(
            md.contains(":information_source: Scope: **full**"),
            "missing full-scope banner:\n{}",
            md
        );
        assert!(
            md.contains("**0** finding(s) across 3 stage(s)"),
            "missing total-findings line:\n{}",
            md
        );
        assert!(
            md.contains(":white_check_mark: **audit**"),
            "audit header: {}",
            md
        );
        assert!(
            md.contains(":white_check_mark: **lint**"),
            "lint header: {}",
            md
        );
        assert!(
            md.contains(":white_check_mark: **test**"),
            "test header: {}",
            md
        );
        // No bullets when no findings.
        assert!(
            !md.contains("- **"),
            "should not render bullets on a clean run:\n{}",
            md
        );
    }

    #[test]
    fn renders_failing_review_with_findings() {
        let mut env = passing_envelope();
        env.summary.passed = false;
        env.summary.status = "failed".to_string();
        env.summary.total_findings = 5;

        // Audit fails with three actionable findings.
        env.audit.passed = false;
        env.audit.exit_code = 1;
        env.audit.finding_count = 3;
        env.audit.output = Some(audit_full_with_findings(vec![
            "ability-shape",
            "ability-shape",
            "naming-convention",
        ]));

        // Lint fails with two sniffs.
        env.lint.passed = false;
        env.lint.exit_code = 1;
        env.lint.finding_count = 2;
        env.lint.output = Some(lint_with_findings(vec![
            ("Squiz.Commenting.FunctionComment.Missing", "no docblock"),
            ("Squiz.Commenting.FunctionComment.Missing", "no docblock"),
        ]));

        let md = render_pr_comment(&env);
        assert!(md.contains(":x: **audit**"), "audit failed icon:\n{}", md);
        assert!(md.contains(":x: **lint**"), "lint failed icon:\n{}", md);
        assert!(
            md.contains("- `src/foo.rs` — **missing_method**: deviates from convention; align with siblings"),
            "audit finding detail:\n{}",
            md
        );
        assert!(
            md.contains("- `Squiz.Commenting.FunctionComment.Missing` — 2 finding(s)"),
            "lint sniff bucket count:\n{}",
            md
        );
    }

    #[test]
    fn renders_optional_ci_stage() {
        let mut env = passing_envelope();
        env.summary.total_findings = 1;
        env.ci_profile = Some(stage_ci_with_jobs(vec![
            ci_job("lint", true, 0),
            ci_job("unit", false, 1),
        ]));

        let md = render_pr_comment(&env);

        assert!(md.contains("**1** finding(s) across 4 stage(s)"));
        assert!(md.contains(":x: **ci**"));
        assert!(md.contains("- `lint` — passed (exit 0)"));
        assert!(md.contains("- `unit` — failed (exit 1)"));
    }

    #[test]
    fn renders_all_stages_skipped() {
        let env = ReviewCommandOutput {
            command: "review".to_string(),
            plan: build_quality_plan(QualityPlanOptions::skipped_review(
                "my-comp",
                "no files changed",
            )),
            observation: None,
            artifact: super::super::build_artifact("my-comp", "main", "abc123", Vec::new()),
            summary: super::super::ReviewSummary {
                passed: true,
                status: "passed".to_string(),
                component: "my-comp".to_string(),
                scope: "changed-since".to_string(),
                changed_since: Some("main".to_string()),
                total_findings: 0,
                changed_file_count: Some(0),
                hints: vec!["No files changed since main — skipping review".to_string()],
            },
            audit: stage_skipped("audit", "no files changed"),
            lint: stage_skipped("lint", "no files changed"),
            test: stage_skipped("test", "no files changed"),
            ci_profile: None,
        };
        let md = render_pr_comment(&env);
        assert!(
            md.contains(":fast_forward: **audit** — skipped (no files changed)"),
            "audit skipped header:\n{}",
            md
        );
        assert!(
            md.contains(":fast_forward: **lint**"),
            "lint skipped: {}",
            md
        );
        assert!(
            md.contains(":fast_forward: **test**"),
            "test skipped: {}",
            md
        );
        assert!(
            md.contains("**0** finding(s) across 0 stage(s)"),
            "ran-stage count should reflect zero ran stages:\n{}",
            md
        );
        assert!(
            !md.contains("Deep dive:"),
            "skipped stages must not emit deep-dive hints:\n{}",
            md
        );
    }

    #[test]
    fn renders_changed_since_scope_banner() {
        let mut env = passing_envelope();
        env.summary.scope = "changed-since".to_string();
        env.summary.changed_since = Some("trunk".to_string());
        let md = render_pr_comment(&env);
        assert!(
            md.contains(":zap: Scope: **changed files only** (since `trunk`)"),
            "missing changed-since banner:\n{}",
            md
        );
    }

    #[test]
    fn renders_full_scope_banner() {
        let env = passing_envelope();
        let md = render_pr_comment(&env);
        assert!(
            md.contains(":information_source: Scope: **full**"),
            "missing full-scope banner:\n{}",
            md
        );
    }

    #[test]
    fn renders_action_banners_before_scope_banner() {
        let env = passing_envelope();
        let banners = vec![
            ("autofix".to_string(), "applied 3 file(s)".to_string()),
            ("binary-source".to_string(), "fallback".to_string()),
            ("custom".to_string(), "value".to_string()),
        ];
        let md = render_pr_comment_with_banners(&env, &banners);

        assert!(md.starts_with("> :wrench: **autofix:** applied 3 file(s)"));
        assert!(md.contains("> :warning: **binary-source:** fallback"));
        assert!(md.contains("> :information_source: **custom:** value"));
        assert!(
            md.find("**custom:** value").unwrap() < md.find("Scope: **full**").unwrap(),
            "banners should render before scope banner:\n{}",
            md
        );
    }

    #[test]
    fn renders_test_pass_count_when_no_failures() {
        let mut env = passing_envelope();
        env.test.output = Some(test_with_counts(42, 0, 1));
        let md = render_pr_comment(&env);
        assert!(
            md.contains("- 42 passed"),
            "missing passed count line:\n{}",
            md
        );
        assert!(md.contains("- 1 skipped"), "missing skipped line:\n{}", md);
    }

    #[test]
    fn renders_test_failure_summary() {
        let mut env = passing_envelope();
        env.test.passed = false;
        env.test.exit_code = 1;
        env.test.finding_count = 3;
        env.test.output = Some(test_with_counts(10, 3, 0));
        let md = render_pr_comment(&env);
        assert!(
            md.contains("- **3 failed** out of 13 total"),
            "missing failure summary:\n{}",
            md
        );
        assert!(md.contains("- 10 passed"), "missing passed line:\n{}", md);
    }

    #[test]
    fn renders_top_test_findings_when_present() {
        let mut env = passing_envelope();
        env.test.passed = false;
        env.test.exit_code = 1;
        env.test.finding_count = 3;
        env.test.output = Some(test_with_findings(3));

        let md = render_pr_comment(&env);
        assert!(
            md.contains("- **3 failed** out of 24 total"),
            "missing aggregate line:\n{}",
            md
        );
        assert!(
            md.contains("- `tests::suite::case_00` — assertion 0 failed (`tests/suite.rs:10`)"),
            "missing failed-test detail line:\n{}",
            md
        );
        assert!(
            md.contains("- `tests::suite::case_02` — assertion 2 failed (`tests/suite.rs:12`)"),
            "missing final failed-test detail line:\n{}",
            md
        );
    }

    #[test]
    fn caps_test_findings_at_top_10() {
        let mut env = passing_envelope();
        env.test.passed = false;
        env.test.exit_code = 1;
        env.test.finding_count = 12;
        env.test.output = Some(test_with_findings(12));

        let md = render_pr_comment(&env);
        let bullet_count = md.matches("- `tests::suite::case_").count();
        assert_eq!(
            bullet_count, TOP_N,
            "should render exactly 10 failed-test bullets:\n{}",
            md
        );
        assert!(
            md.contains("_… 2 more failed test(s)_"),
            "missing failed-test overflow hint:\n{}",
            md
        );
        assert!(
            !md.contains("tests::suite::case_10"),
            "should not render failed tests beyond top cap:\n{}",
            md
        );
    }

    #[test]
    fn caps_lint_sniffs_at_top_10() {
        // 15 distinct categories — expect 10 rendered + a "5 more" line.
        let items: Vec<(String, String)> = (0..15)
            .map(|i| (format!("Sniff.Code.{:02}", i), format!("msg {}", i)))
            .collect();
        let item_refs: Vec<(&str, &str)> = items
            .iter()
            .map(|(c, m)| (c.as_str(), m.as_str()))
            .collect();

        let mut env = passing_envelope();
        env.lint.passed = false;
        env.lint.exit_code = 1;
        env.lint.finding_count = 15;
        env.lint.output = Some(lint_with_findings(item_refs));

        let md = render_pr_comment(&env);
        let bullet_count = md.matches("- `Sniff.Code.").count();
        assert_eq!(
            bullet_count, TOP_N,
            "should render exactly 10 sniff bullets, got {}:\n{}",
            bullet_count, md
        );
        assert!(
            md.contains("_… 5 more sniff(s)_"),
            "missing overflow hint:\n{}",
            md
        );
    }

    #[test]
    fn caps_audit_findings_at_top_10() {
        let conventions: Vec<String> = (0..15).map(|i| format!("convention-{:02}", i)).collect();
        let conv_refs: Vec<&str> = conventions.iter().map(|s| s.as_str()).collect();

        let mut env = passing_envelope();
        env.audit.passed = false;
        env.audit.exit_code = 1;
        env.audit.finding_count = 15;
        env.audit.output = Some(audit_full_with_findings(conv_refs));

        let md = render_pr_comment(&env);
        let bullet_count = md.matches("- `src/foo.rs` — **missing_method**").count();
        assert_eq!(
            bullet_count, TOP_N,
            "should render exactly 10 audit finding bullets:\n{}",
            md
        );
        assert!(
            md.contains("_… 5 more audit finding(s)_"),
            "missing overflow hint:\n{}",
            md
        );
    }

    #[test]
    fn renders_deep_dive_hints_for_ran_stages() {
        let env = passing_envelope();
        let md = render_pr_comment(&env);
        assert!(md.contains("> Deep dive: homeboy audit my-comp"));
        assert!(md.contains("> Deep dive: homeboy lint my-comp"));
        assert!(md.contains("> Deep dive: homeboy test my-comp"));
    }

    #[test]
    fn never_emits_top_level_section_heading() {
        let env = passing_envelope();
        let md = render_pr_comment(&env);
        // The consumer (`homeboy git pr comment --header`) owns the wrapping
        // `### Title` heading. Renderer must only emit the body.
        assert!(
            !md.starts_with("### "),
            "renderer must not emit a section header:\n{}",
            md
        );
    }
}
