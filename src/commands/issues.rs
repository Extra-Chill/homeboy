//! `homeboy issues reconcile` — finding-stream → tracker reconciliation.
//!
//! See homeboy issue #1551 for the architectural framing. This is the CLI
//! surface that the action's `auto-file-categorized-issues.sh` collapses
//! to a single call against.

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;

use homeboy::core::code_audit::FindingConfidence;
use homeboy::core::issues::{
    apply_plan, build_findings_from_native_output, reconcile_scoped, GithubTracker,
    IssueRenderContext, ReconcileConfig, ReconcileFindingsInput, ReconcilePlan, ReconcileResult,
    Tracker,
};

use super::parse_key_val;
use super::CmdResult;

#[derive(Args)]
pub struct IssuesArgs {
    #[command(subcommand)]
    command: IssuesCommand,
}

#[derive(Subcommand)]
enum IssuesCommand {
    /// Reconcile a finding stream against an issue tracker.
    ///
    /// Reads structured findings (from `homeboy audit --json-summary` or
    /// `homeboy lint --json` or any equivalent), inspects open and closed
    /// issues on the tracker, and produces a deterministic plan: file new,
    /// update, close, dedupe, or skip per category.
    ///
    /// Defaults to dry-run; pass `--apply` to actually call the tracker.
    Reconcile {
        /// Component ID. Tracker repo is resolved from this component's
        /// `remote_url` (or git remote, when --path is set).
        component_id: String,

        /// Tracker URI. Currently only `github://owner/repo` is supported.
        /// When omitted, defaults to the component's GitHub remote — the
        /// common case.
        #[arg(long, value_name = "URI")]
        tracker: Option<String>,

        /// Path to a JSON findings file. Use `-` to read from stdin. The
        /// file's shape:
        ///
        /// ```json
        /// {
        ///   "command": "audit",
        ///   "groups": {
        ///     "unreferenced_export": { "count": 57, "label": "unreferenced export", "body": "..." },
        ///     "god_file": { "count": 23, "label": "god file", "body": "..." }
        ///   }
        /// }
        /// ```
        ///
        /// Categories with `count: 0` drive close-on-resolved transitions.
        /// `body` is rendered as-is into new or updated issues — callers
        /// own the finding-table format.
        #[arg(long, value_name = "PATH")]
        findings: Option<String>,

        /// Native Homeboy command output to normalize before reconcile.
        /// Repeatable as `--from-output audit=/tmp/audit.json`.
        #[arg(long = "from-output", value_name = "COMMAND=PATH", value_parser = parse_key_val)]
        from_output: Vec<(String, String)>,

        /// Optional run URL appended to generated issue bodies when using
        /// `--from-output`.
        #[arg(long, value_name = "URL")]
        run_url: Option<String>,

        /// Don't refresh the body of closed-not_planned issues with the
        /// latest finding count. Default is to refresh (so the closed
        /// issue stays useful as a "current state" reference).
        #[arg(long)]
        no_refresh_closed: bool,

        /// Cap the number of issues fetched from the tracker for dedup
        /// analysis. Defaults to 200 — high enough for normal repos, but
        /// avoids paginating the entire tracker.
        #[arg(long, default_value_t = 200)]
        list_limit: usize,

        /// Actually perform the reconcile actions. Default is dry-run.
        #[arg(long)]
        apply: bool,

        /// Workspace path to discover the component from a portable
        /// homeboy.json (CI runners, ad-hoc clones).
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },

    /// Convert native command output into the canonical reconcile input shape.
    BuildFindings {
        /// Native Homeboy command output to normalize. Repeatable as
        /// `--from-output audit=/tmp/audit.json`.
        #[arg(long = "from-output", value_name = "COMMAND=PATH", value_parser = parse_key_val)]
        from_output: Vec<(String, String)>,

        /// Optional run URL appended to generated issue bodies.
        #[arg(long, value_name = "URL")]
        run_url: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum IssuesCommandOutput {
    Reconcile(ReconcileOutput),
    BuildFindings(ReconcileFindingsInput),
}

/// What the CLI emits for `homeboy issues reconcile`. Both dry-run and
/// apply runs share this shape; `applied = false` means dry-run, no
/// tracker calls were made.
#[derive(Serialize)]
pub struct ReconcileOutput {
    pub component_id: String,
    pub command: String,
    pub applied: bool,
    /// Always populated — same shape regardless of dry-run vs apply.
    pub plan_summary: ReconcileOutputSummary,
    /// Only populated when `applied = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ReconcileResult>,
    /// Always populated — full plan as a list of human-readable lines.
    pub plan_lines: Vec<String>,
}

#[derive(Serialize, Default)]
pub struct ReconcileOutputSummary {
    pub total_actions: usize,
    pub file_new: usize,
    pub update: usize,
    pub update_closed: usize,
    pub close: usize,
    pub close_duplicate: usize,
    pub skip: usize,
}

pub fn run(args: IssuesArgs, _global: &super::GlobalArgs) -> CmdResult<IssuesCommandOutput> {
    match args.command {
        IssuesCommand::Reconcile {
            component_id,
            tracker: _tracker,
            findings,
            from_output,
            run_url,
            no_refresh_closed,
            list_limit,
            apply,
            path,
        } => {
            let findings_input = read_reconcile_input(findings.as_deref(), &from_output, run_url)?;
            let command_label = findings_input.command.clone();
            let groups = into_issue_groups(findings_input, &component_id);

            let config = build_reconcile_config(no_refresh_closed);

            // Default tracker = GitHub against the component's remote.
            let tracker_impl = GithubTracker::new(component_id.clone()).with_path(path.clone());

            // Fetch existing issues for label-scoping.
            let existing = tracker_impl.list_issues(&command_label, list_limit)?;

            // Pure decision.
            let plan = reconcile_scoped(&groups, &existing, &config, &command_label, &component_id);
            let plan_lines = render_plan_lines(&plan);
            let plan_summary = summarize_plan(&plan);

            if apply {
                let result = apply_plan(plan, &tracker_impl)?;
                let exit = if result.failed_count > 0 { 1 } else { 0 };
                let output = ReconcileOutput {
                    component_id,
                    command: command_label,
                    applied: true,
                    plan_summary,
                    result: Some(result),
                    plan_lines,
                };
                Ok((IssuesCommandOutput::Reconcile(output), exit))
            } else {
                let output = ReconcileOutput {
                    component_id,
                    command: command_label,
                    applied: false,
                    plan_summary,
                    result: None,
                    plan_lines,
                };
                Ok((IssuesCommandOutput::Reconcile(output), 0))
            }
        }
        IssuesCommand::BuildFindings {
            from_output,
            run_url,
        } => {
            let findings_input = build_findings_input(&from_output, run_url)?;
            Ok((IssuesCommandOutput::BuildFindings(findings_input), 0))
        }
    }
}

// ---------------------------------------------------------------------------
// Findings input parsing
// ---------------------------------------------------------------------------

/// Findings input shape. Designed to be a minimal superset of the JSON the
/// action's bash already produces, so the migration path doesn't require
/// changing the audit/lint/test output formats.
fn into_issue_groups(
    input: ReconcileFindingsInput,
    component_id: &str,
) -> Vec<homeboy::core::issues::IssueGroup> {
    input
        .groups
        .into_iter()
        .map(|(category, row)| homeboy::core::issues::IssueGroup {
            command: input.command.clone(),
            component_id: component_id.to_string(),
            category,
            count: row.count,
            label: row.label,
            body: row.body,
            confidence: row.confidence,
        })
        .collect()
}

fn read_reconcile_input(
    findings: Option<&str>,
    from_output: &[(String, String)],
    run_url: Option<String>,
) -> homeboy::core::Result<ReconcileFindingsInput> {
    match (findings, from_output.is_empty()) {
        (Some(path), true) => read_findings(path),
        (None, false) => build_findings_input(from_output, run_url),
        (Some(_), false) => Err(homeboy::core::Error::validation_invalid_argument(
            "findings",
            "Use either --findings or --from-output, not both",
            None,
            None,
        )),
        (None, true) => Err(homeboy::core::Error::validation_invalid_argument(
            "findings",
            "Missing --findings or --from-output",
            None,
            Some(vec![
                "Pass --findings <path> for pre-rendered input".to_string(),
                "Pass --from-output audit=<path> to normalize native command output".to_string(),
            ]),
        )),
    }
}

fn build_findings_input(
    from_output: &[(String, String)],
    run_url: Option<String>,
) -> homeboy::core::Result<ReconcileFindingsInput> {
    if from_output.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "from-output",
            "At least one --from-output COMMAND=PATH pair is required",
            None,
            None,
        ));
    }

    let context = IssueRenderContext { run_url };
    let mut merged = ReconcileFindingsInput::default();
    let mut command_label: Option<&str> = None;
    for (command, path) in from_output {
        if let Some(existing) = command_label {
            if existing != command {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "from-output",
                    "Multiple command labels in one issue reconcile input are not supported yet",
                    None,
                    Some(vec![
                        "Run one reconcile per command label for now".to_string(),
                        "Use repeated --from-output only to merge split output files from the same command".to_string(),
                    ]),
                ));
            }
        } else {
            command_label = Some(command);
        }
        let value = read_json_value(path, "native command output")?;
        let rendered = build_findings_from_native_output(command, value, &context)?;
        merged.merge(rendered);
    }
    Ok(merged)
}

fn read_findings(path: &str) -> homeboy::core::Result<ReconcileFindingsInput> {
    let value = read_json_value(path, "findings")?;
    parse_findings_value(value)
}

fn read_json_value(path: &str, label: &str) -> homeboy::core::Result<Value> {
    let raw = if path == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|e| {
            homeboy::core::Error::internal_io(
                format!("read {} from stdin: {}", label, e),
                Some("stdin".into()),
            )
        })?;
        buf
    } else {
        std::fs::read_to_string(path).map_err(|e| {
            homeboy::core::Error::internal_io(
                format!("read {} file: {}", label, e),
                Some(path.to_string()),
            )
        })?
    };

    let value: Value = serde_json::from_str(&raw).map_err(|e| {
        homeboy::core::Error::validation_invalid_json(
            e,
            Some("parse findings JSON".to_string()),
            Some(raw.chars().take(200).collect()),
        )
    })?;

    Ok(value)
}

fn parse_findings_value(value: Value) -> homeboy::core::Result<ReconcileFindingsInput> {
    let obj = value.as_object().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "findings",
            "Findings JSON must be an object with a `command` and `groups` field",
            None,
            None,
        )
    })?;

    let command = obj
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "findings.command",
                "Missing or non-string `command` field (e.g. \"audit\")",
                None,
                None,
            )
        })?
        .to_string();

    let mut groups: BTreeMap<String, homeboy::core::issues::RenderedIssueGroup> = BTreeMap::new();
    if let Some(groups_value) = obj.get("groups") {
        let groups_obj = groups_value.as_object().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "findings.groups",
                "`groups` must be a JSON object keyed by category",
                None,
                None,
            )
        })?;
        for (category, row_value) in groups_obj {
            let row_obj = row_value.as_object().ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    format!("findings.groups.{}", category),
                    "Each group must be a JSON object with `count`, optional `label`, optional `body`, optional `confidence`",
                    None,
                    None,
                )
            })?;
            let count = row_obj
                .get("count")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(0);
            let label = row_obj
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let body = row_obj
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let confidence = row_obj
                .get("confidence")
                .and_then(|v| v.as_str())
                .and_then(parse_confidence);
            groups.insert(
                category.clone(),
                homeboy::core::issues::RenderedIssueGroup {
                    count,
                    label,
                    body,
                    confidence,
                },
            );
        }
    }

    Ok(ReconcileFindingsInput { command, groups })
}

fn parse_confidence(raw: &str) -> Option<FindingConfidence> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "structural" => Some(FindingConfidence::Structural),
        "graph" => Some(FindingConfidence::Graph),
        "heuristic" => Some(FindingConfidence::Heuristic),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// homeboy.json suppression read
// ---------------------------------------------------------------------------

fn build_reconcile_config(no_refresh_closed: bool) -> ReconcileConfig {
    ReconcileConfig {
        refresh_closed_not_planned: !no_refresh_closed,
        ..ReconcileConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Plan rendering helpers (also used by apply path for symmetry)
// ---------------------------------------------------------------------------

fn render_plan_lines(plan: &ReconcilePlan) -> Vec<String> {
    plan.actions
        .iter()
        .map(|a| match a {
            homeboy::core::issues::ReconcileAction::FileNew {
                command,
                component_id,
                category,
                count,
                ..
            } => format!(
                "file_new      {}: {} in {} ({})",
                command, category, component_id, count
            ),
            homeboy::core::issues::ReconcileAction::Update {
                number,
                category,
                count,
                ..
            } => format!("update        {} ({}) → #{}", category, count, number),
            homeboy::core::issues::ReconcileAction::UpdateClosed {
                number,
                category,
                count,
                ..
            } => format!(
                "update_closed {} ({}) → #{} (stays closed)",
                category, count, number
            ),
            homeboy::core::issues::ReconcileAction::Close {
                number, category, ..
            } => format!("close         {} → #{}", category, number),
            homeboy::core::issues::ReconcileAction::CloseDuplicate {
                number,
                keep,
                category,
                ..
            } => format!(
                "dedupe        {} → keep #{}, close #{}",
                category, keep, number
            ),
            homeboy::core::issues::ReconcileAction::Skip {
                category, reason, ..
            } => format!("skip          {} ({:?})", category, reason),
        })
        .collect()
}

fn summarize_plan(plan: &ReconcilePlan) -> ReconcileOutputSummary {
    let counts = plan.counts();
    ReconcileOutputSummary {
        total_actions: plan.actions.len(),
        file_new: counts.file_new,
        update: counts.update,
        update_closed: counts.update_closed,
        close: counts.close,
        close_duplicate: counts.close_duplicate,
        skip: counts.skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_findings_accepts_confidence_per_group() {
        let input = serde_json::json!({
            "command": "audit",
            "groups": {
                "god_file": {
                    "count": 2,
                    "label": "god file",
                    "body": "body",
                    "confidence": "heuristic"
                }
            }
        });

        let parsed = parse_findings_value(input).unwrap();
        let groups = into_issue_groups(parsed, "homeboy");

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].confidence, Some(FindingConfidence::Heuristic));
    }

    #[test]
    fn reconcile_config_only_controls_closed_refresh_behavior() {
        let config = build_reconcile_config(true);

        assert!(!config.refresh_closed_not_planned);
    }
}
