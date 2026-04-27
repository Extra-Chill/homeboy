//! Render native Homeboy command output into issue-reconcile groups.
//!
//! This is the pre-reconcile contract: command outputs (`audit`, `lint`,
//! `test`) become grouped issue bodies once, then the reconciler decides how
//! to apply them against a tracker.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::code_audit::FindingConfidence;

/// Canonical input shape consumed by `homeboy issues reconcile`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcileFindingsInput {
    pub command: String,
    #[serde(default)]
    pub groups: BTreeMap<String, RenderedIssueGroup>,
}

/// One rendered category row in the reconcile input.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RenderedIssueGroup {
    pub count: usize,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<FindingConfidence>,
}

/// Optional context appended to generated issue bodies.
#[derive(Debug, Clone, Default)]
pub struct IssueRenderContext {
    pub run_url: Option<String>,
}

impl ReconcileFindingsInput {
    pub fn merge(&mut self, other: ReconcileFindingsInput) {
        if self.command.is_empty() {
            self.command = other.command;
        }
        for (category, group) in other.groups {
            self.groups.insert(category, group);
        }
    }
}

/// Build reconcile input from one native command output envelope.
pub fn build_findings_from_native_output(
    command: &str,
    output: Value,
    context: &IssueRenderContext,
) -> crate::Result<ReconcileFindingsInput> {
    let data = output.get("data").unwrap_or(&output);
    match command {
        "audit" => Ok(render_audit(data, context)),
        "lint" => Ok(render_lint(data, context)),
        "test" => Ok(render_test(data, context)),
        other => Err(crate::Error::validation_invalid_argument(
            "command",
            format!("Unsupported native issue output command `{}`", other),
            None,
            Some(vec!["Supported commands: audit, lint, test".to_string()]),
        )),
    }
}

fn render_audit(data: &Value, context: &IssueRenderContext) -> ReconcileFindingsInput {
    let mut by_kind: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let findings = data
        .get("findings")
        .and_then(Value::as_array)
        .or_else(|| data.pointer("/top_findings").and_then(Value::as_array));
    if let Some(findings) = findings {
        for finding in findings {
            let kind = finding
                .get("kind")
                .and_then(Value::as_str)
                .or_else(|| finding.get("convention").and_then(Value::as_str))
                .unwrap_or("audit_finding");
            by_kind.entry(kind.to_string()).or_default().push(finding);
        }
    }

    let mut groups = BTreeMap::new();
    for (kind, findings) in by_kind {
        let fixability = data.pointer(&format!("/fixability/by_kind/{}", kind));
        let confidence = findings
            .iter()
            .find_map(|f| f.get("confidence").and_then(Value::as_str))
            .and_then(|raw| serde_json::from_value(Value::String(raw.to_string())).ok());
        groups.insert(
            kind.clone(),
            RenderedIssueGroup {
                count: findings.len(),
                label: labelize(&kind),
                body: render_audit_body(&kind, &findings, fixability, context),
                confidence,
            },
        );
    }

    ReconcileFindingsInput {
        command: "audit".to_string(),
        groups,
    }
}

fn render_audit_body(
    kind: &str,
    findings: &[&Value],
    fixability: Option<&Value>,
    context: &IssueRenderContext,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## {}", labelize(kind));
    let _ = writeln!(out);
    let _ = writeln!(out, "{} audit finding(s) in this category.", findings.len());
    if let Some(url) = context.run_url.as_deref() {
        let _ = writeln!(out, "\nRun: {}", url);
    }
    render_fixability(&mut out, fixability);
    let _ = writeln!(out, "\n### Findings");

    for finding in findings.iter().take(20) {
        let file = finding
            .get("file")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let description = finding
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("audit finding");
        let suggestion = finding.get("suggestion").and_then(Value::as_str);
        let _ = writeln!(out, "- `{}` — {}", file, description);
        if let Some(suggestion) = suggestion {
            let _ = writeln!(out, "  - Suggested fix: {}", suggestion);
        }
    }
    if findings.len() > 20 {
        let _ = writeln!(out, "- _... {} more finding(s)_", findings.len() - 20);
    }
    out
}

fn render_lint(data: &Value, context: &IssueRenderContext) -> ReconcileFindingsInput {
    let mut by_category: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    if let Some(findings) = data.get("lint_findings").and_then(Value::as_array) {
        for finding in findings {
            let category = finding
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("lint_finding");
            by_category
                .entry(category.to_string())
                .or_default()
                .push(finding);
        }
    }

    if by_category.is_empty() && !data.get("passed").and_then(Value::as_bool).unwrap_or(true) {
        by_category.insert("lint_failure".to_string(), Vec::new());
    }

    let mut groups = BTreeMap::new();
    for (category, findings) in by_category {
        let count = findings.len().max(1);
        groups.insert(
            category.clone(),
            RenderedIssueGroup {
                count,
                label: labelize(&category),
                body: render_lint_body(&category, &findings, data, context),
                confidence: None,
            },
        );
    }

    ReconcileFindingsInput {
        command: "lint".to_string(),
        groups,
    }
}

fn render_lint_body(
    category: &str,
    findings: &[&Value],
    data: &Value,
    context: &IssueRenderContext,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## {}", labelize(category));
    let _ = writeln!(out);
    if findings.is_empty() {
        let status = data
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        let exit = data.get("exit_code").and_then(Value::as_i64).unwrap_or(1);
        let _ = writeln!(
            out,
            "Lint {} without structured findings (exit {}).",
            status, exit
        );
        if let Some(url) = context.run_url.as_deref() {
            let _ = writeln!(out, "\nRun: {}", url);
        }
        return out;
    }

    let _ = writeln!(out, "{} lint finding(s) in this category.", findings.len());
    if let Some(url) = context.run_url.as_deref() {
        let _ = writeln!(out, "\nRun: {}", url);
    }
    let _ = writeln!(out, "\n### Findings");
    for finding in findings.iter().take(20) {
        let id = finding.get("id").and_then(Value::as_str).unwrap_or("lint");
        let message = finding
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("lint finding");
        let _ = writeln!(out, "- `{}` — {}", id, message);
    }
    if findings.len() > 20 {
        let _ = writeln!(out, "- _... {} more finding(s)_", findings.len() - 20);
    }
    out
}

fn render_test(data: &Value, context: &IssueRenderContext) -> ReconcileFindingsInput {
    let mut groups = BTreeMap::new();

    if let Some(clusters) = data.pointer("/analysis/clusters").and_then(Value::as_array) {
        for cluster in clusters {
            let category = cluster
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("test_failure");
            let count = cluster.get("count").and_then(Value::as_u64).unwrap_or(1) as usize;
            insert_test_group(
                &mut groups,
                category,
                count,
                render_test_cluster_body(category, cluster, context),
            );
        }
    }

    if groups.is_empty() {
        let failed = data
            .pointer("/test_counts/failed")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if failed > 0 || !data.get("passed").and_then(Value::as_bool).unwrap_or(true) {
            insert_test_group(
                &mut groups,
                "test_failure",
                failed.max(1),
                render_test_fallback_body(data, context),
            );
        }
    }

    ReconcileFindingsInput {
        command: "test".to_string(),
        groups,
    }
}

fn render_test_cluster_body(
    category: &str,
    cluster: &Value,
    context: &IssueRenderContext,
) -> String {
    let mut out = String::new();
    let count = cluster.get("count").and_then(Value::as_u64).unwrap_or(1);
    let _ = writeln!(out, "## {}", labelize(category));
    let _ = writeln!(out);
    let _ = writeln!(out, "{} test failure(s) in this cluster.", count);
    if let Some(url) = context.run_url.as_deref() {
        let _ = writeln!(out, "\nRun: {}", url);
    }
    if let Some(pattern) = cluster.get("pattern").and_then(Value::as_str) {
        let _ = writeln!(out, "\n**Pattern:** {}", pattern);
    }
    if let Some(fix) = cluster.get("suggested_fix").and_then(Value::as_str) {
        let _ = writeln!(out, "\n**Suggested fix:** {}", fix);
    }
    if let Some(files) = cluster.get("affected_files").and_then(Value::as_array) {
        let _ = writeln!(out, "\n### Affected files");
        for file in files.iter().filter_map(Value::as_str).take(20) {
            let _ = writeln!(out, "- `{}`", file);
        }
    }
    if let Some(tests) = cluster.get("example_tests").and_then(Value::as_array) {
        let _ = writeln!(out, "\n### Example tests");
        for test in tests.iter().filter_map(Value::as_str).take(10) {
            let _ = writeln!(out, "- `{}`", test);
        }
    }
    out
}

fn render_test_fallback_body(data: &Value, context: &IssueRenderContext) -> String {
    let mut out = String::new();
    let failed = data
        .pointer("/test_counts/failed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = data
        .pointer("/test_counts/total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let exit = data.get("exit_code").and_then(Value::as_i64).unwrap_or(1);
    let _ = writeln!(out, "## Test failure");
    let _ = writeln!(out);
    if failed > 0 || total > 0 {
        let _ = writeln!(out, "{} failed test(s) out of {} total.", failed, total);
    } else {
        let _ = writeln!(
            out,
            "Test phase failed without structured counts (exit {}).",
            exit
        );
    }
    if let Some(url) = context.run_url.as_deref() {
        let _ = writeln!(out, "\nRun: {}", url);
    }
    out
}

fn render_fixability(out: &mut String, fixability: Option<&Value>) {
    let Some(fixability) = fixability else {
        return;
    };
    let automated = fixability
        .get("automated")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let manual = fixability
        .get("manual_only")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = fixability.get("total").and_then(Value::as_u64).unwrap_or(0);
    let _ = writeln!(out, "\n### Autofix status");
    let _ = writeln!(out, "- Total fixable: {}", total);
    let _ = writeln!(out, "- Automated: {}", automated);
    let _ = writeln!(out, "- Manual-only: {}", manual);
}

fn insert_test_group(
    groups: &mut BTreeMap<String, RenderedIssueGroup>,
    category: &str,
    count: usize,
    body: String,
) {
    groups.insert(
        category.to_string(),
        RenderedIssueGroup {
            count,
            label: labelize(category),
            body,
            confidence: None,
        },
    );
}

fn labelize(raw: &str) -> String {
    raw.replace(['_', '-'], " ")
}

#[cfg(test)]
#[path = "../../../tests/core/issues/render_test.rs"]
mod render_test;
