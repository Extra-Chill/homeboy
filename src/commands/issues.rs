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
use std::path::PathBuf;

use homeboy::code_audit::FindingConfidence;
use homeboy::issues::{
    apply_plan, reconcile, GithubTracker, IssueGroup, ReconcileConfig, ReconcilePlan,
    ReconcileResult, Tracker,
};

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
        findings: String,

        /// Read suppressions from `homeboy.json`'s `audit.suppressed_categories`
        /// and `issues.suppression_labels`. When false, suppression must be
        /// passed explicitly via the flags below.
        #[arg(long, default_value_t = true)]
        suppress_from_config: bool,

        /// Override category suppressions (repeatable). Replaces the
        /// homeboy.json list when both are set.
        #[arg(long, value_name = "CATEGORY")]
        suppress_category: Vec<String>,

        /// Override label suppressions (repeatable). Replaces the
        /// homeboy.json list when both are set.
        #[arg(long, value_name = "LABEL")]
        suppress_label: Vec<String>,

        /// Override review-only categories (repeatable). Replaces the default
        /// heuristic/threshold list and homeboy.json's
        /// `issues.review_only_categories` when set.
        #[arg(long, value_name = "CATEGORY")]
        review_only_category: Vec<String>,

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
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum IssuesCommandOutput {
    Reconcile(ReconcileOutput),
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
    pub plan_summary: PlanSummary,
    /// Only populated when `applied = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ReconcileResult>,
    /// Always populated — full plan as a list of human-readable lines.
    pub plan_lines: Vec<String>,
}

#[derive(Serialize, Default)]
pub struct PlanSummary {
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
            suppress_from_config,
            suppress_category,
            suppress_label,
            review_only_category,
            no_refresh_closed,
            list_limit,
            apply,
            path,
        } => {
            let findings_input = read_findings(&findings)?;
            let command_label = findings_input.command.clone();
            let groups = findings_input.into_groups(&component_id);

            // Build reconcile config: CLI overrides take priority; otherwise
            // read homeboy.json when the flag is set; otherwise empty.
            let config = build_reconcile_config(
                &component_id,
                path.as_deref(),
                suppress_from_config,
                suppress_category,
                suppress_label,
                review_only_category,
                no_refresh_closed,
            )?;

            // Default tracker = GitHub against the component's remote.
            let tracker_impl = GithubTracker::new(component_id.clone()).with_path(path.clone());

            // Fetch existing issues for label-scoping.
            let existing = tracker_impl.list_issues(&command_label, list_limit)?;

            // Pure decision.
            let plan = reconcile(&groups, &existing, &config);
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
    }
}

// ---------------------------------------------------------------------------
// Findings input parsing
// ---------------------------------------------------------------------------

/// Findings input shape. Designed to be a minimal superset of the JSON the
/// action's bash already produces, so the migration path doesn't require
/// changing the audit/lint/test output formats.
#[derive(Debug)]
struct FindingsInput {
    command: String,
    groups: BTreeMap<String, GroupRow>,
}

#[derive(Debug, Default)]
struct GroupRow {
    count: usize,
    label: String,
    body: String,
    confidence: Option<FindingConfidence>,
}

impl FindingsInput {
    fn into_groups(self, component_id: &str) -> Vec<IssueGroup> {
        self.groups
            .into_iter()
            .map(|(category, row)| IssueGroup {
                command: self.command.clone(),
                component_id: component_id.to_string(),
                category,
                count: row.count,
                label: row.label,
                body: row.body,
                confidence: row.confidence,
            })
            .collect()
    }
}

fn read_findings(path: &str) -> homeboy::Result<FindingsInput> {
    let raw = if path == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|e| {
            homeboy::Error::internal_io(
                format!("read findings from stdin: {}", e),
                Some("stdin".into()),
            )
        })?;
        buf
    } else {
        std::fs::read_to_string(path).map_err(|e| {
            homeboy::Error::internal_io(
                format!("read findings file: {}", e),
                Some(path.to_string()),
            )
        })?
    };

    let value: Value = serde_json::from_str(&raw).map_err(|e| {
        homeboy::Error::validation_invalid_json(
            e,
            Some("parse findings JSON".to_string()),
            Some(raw.chars().take(200).collect()),
        )
    })?;

    parse_findings_value(value)
}

fn parse_findings_value(value: Value) -> homeboy::Result<FindingsInput> {
    let obj = value.as_object().ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
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
            homeboy::Error::validation_invalid_argument(
                "findings.command",
                "Missing or non-string `command` field (e.g. \"audit\")",
                None,
                None,
            )
        })?
        .to_string();

    let mut groups: BTreeMap<String, GroupRow> = BTreeMap::new();
    if let Some(groups_value) = obj.get("groups") {
        let groups_obj = groups_value.as_object().ok_or_else(|| {
            homeboy::Error::validation_invalid_argument(
                "findings.groups",
                "`groups` must be a JSON object keyed by category",
                None,
                None,
            )
        })?;
        for (category, row_value) in groups_obj {
            let row_obj = row_value.as_object().ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    &format!("findings.groups.{}", category),
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
                GroupRow {
                    count,
                    label,
                    body,
                    confidence,
                },
            );
        }
    }

    Ok(FindingsInput { command, groups })
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

fn build_reconcile_config(
    component_id: &str,
    path: Option<&str>,
    suppress_from_config: bool,
    cli_categories: Vec<String>,
    cli_labels: Vec<String>,
    cli_review_only_categories: Vec<String>,
    no_refresh_closed: bool,
) -> homeboy::Result<ReconcileConfig> {
    let mut config = ReconcileConfig {
        refresh_closed_not_planned: !no_refresh_closed,
        ..ReconcileConfig::default()
    };

    if suppress_from_config {
        if let Some(reconcile_config) = read_suppressions(component_id, path)? {
            let (suppressed, labels, review_only) = reconcile_config;
            config.suppressed_categories = suppressed;
            config.suppression_labels = labels;
            if let Some(review_only) = review_only {
                config.review_only_categories = review_only;
            }
        }
    }

    // CLI flags override homeboy.json when present.
    if !cli_categories.is_empty() {
        config.suppressed_categories = cli_categories;
    }
    if !cli_labels.is_empty() {
        config.suppression_labels = cli_labels;
    }
    if !cli_review_only_categories.is_empty() {
        config.review_only_categories = cli_review_only_categories;
    }

    // Sane default for suppression_labels when neither config nor CLI set
    // them. Mirrors the documented defaults in #1551.
    if config.suppression_labels.is_empty() {
        config.suppression_labels = vec![
            "wontfix".into(),
            "upstream-bug".into(),
            "audit-suppressed".into(),
        ];
    }

    Ok(config)
}

fn read_suppressions(
    component_id: &str,
    path: Option<&str>,
) -> homeboy::Result<Option<(Vec<String>, Vec<String>, Option<Vec<String>>)>> {
    let component_dir = match path {
        Some(p) => PathBuf::from(p),
        None => match homeboy::component::resolve_effective(Some(component_id), None, None) {
            Ok(c) => PathBuf::from(c.local_path),
            Err(_) => return Ok(None),
        },
    };

    let raw = match homeboy::component::read_portable_config(&component_dir)? {
        Some(v) => v,
        None => return Ok(None),
    };

    let categories = raw
        .pointer("/audit/suppressed_categories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let labels = raw
        .pointer("/issues/suppression_labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let review_only = raw
        .pointer("/issues/review_only_categories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        });

    if categories.is_empty() && labels.is_empty() && review_only.is_none() {
        Ok(None)
    } else {
        Ok(Some((categories, labels, review_only)))
    }
}

// ---------------------------------------------------------------------------
// Plan rendering helpers (also used by apply path for symmetry)
// ---------------------------------------------------------------------------

fn render_plan_lines(plan: &ReconcilePlan) -> Vec<String> {
    plan.actions
        .iter()
        .map(|a| match a {
            homeboy::issues::ReconcileAction::FileNew {
                command,
                component_id,
                category,
                count,
                ..
            } => format!(
                "file_new      {}: {} in {} ({})",
                command, category, component_id, count
            ),
            homeboy::issues::ReconcileAction::Update {
                number,
                category,
                count,
                ..
            } => format!("update        {} ({}) → #{}", category, count, number),
            homeboy::issues::ReconcileAction::UpdateClosed {
                number,
                category,
                count,
                ..
            } => format!(
                "update_closed {} ({}) → #{} (stays closed)",
                category, count, number
            ),
            homeboy::issues::ReconcileAction::Close {
                number, category, ..
            } => format!("close         {} → #{}", category, number),
            homeboy::issues::ReconcileAction::CloseDuplicate {
                number,
                keep,
                category,
                ..
            } => format!(
                "dedupe        {} → keep #{}, close #{}",
                category, keep, number
            ),
            homeboy::issues::ReconcileAction::Skip {
                category, reason, ..
            } => format!("skip          {} ({:?})", category, reason),
        })
        .collect()
}

fn summarize_plan(plan: &ReconcilePlan) -> PlanSummary {
    let counts = plan.counts();
    PlanSummary {
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
        let groups = parsed.into_groups("homeboy");

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].confidence, Some(FindingConfidence::Heuristic));
    }

    #[test]
    fn default_reconcile_config_marks_thresholds_and_heuristics_review_only() {
        let config = ReconcileConfig::default();

        assert!(config.review_only_categories.contains(&"god_file".into()));
        assert!(config
            .review_only_categories
            .contains(&"directory_sprawl".into()));
        assert!(config
            .review_only_categories
            .contains(&"missing_test_file".into()));
        assert!(config
            .review_only_categories
            .contains(&"parallel_implementation".into()));
        assert!(config
            .review_only_categories
            .contains(&"unused_parameter".into()));
        assert!(!config
            .review_only_categories
            .contains(&"unreferenced_export".into()));
        assert!(!config
            .review_only_categories
            .contains(&"compiler_warning".into()));
    }
}
