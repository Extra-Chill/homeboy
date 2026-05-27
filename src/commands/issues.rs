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
use std::path::{Path, PathBuf};

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

    /// Reconcile all structured command outputs in one CI run.
    ///
    /// Discovers `<command>.json` files in an output directory, runs the
    /// existing per-command reconcile pipeline, and returns aggregate totals
    /// suitable for GitHub Action consumption.
    ReconcileRun {
        /// Default component ID. Per-output component metadata overrides this
        /// when present in the command JSON.
        component_id: String,

        /// Directory containing structured command outputs such as
        /// `audit.json`, `lint.json`, and `test.json`. Defaults to
        /// HOMEBOY_OUTPUT_DIR when omitted.
        #[arg(long, value_name = "DIR")]
        output_dir: Option<String>,

        /// Comma-separated command list to inspect in the output directory.
        #[arg(long, value_delimiter = ',', default_value = "audit,lint,test")]
        commands: Vec<String>,

        /// Optional run URL appended to generated issue bodies.
        #[arg(long, value_name = "URL")]
        run_url: Option<String>,

        /// Don't refresh the body of closed-not_planned issues with the
        /// latest finding count.
        #[arg(long)]
        no_refresh_closed: bool,

        /// Cap the number of issues fetched from the tracker for dedup
        /// analysis per command.
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
    ReconcileRun(ReconcileRunOutput),
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

#[derive(Serialize)]
pub struct ReconcileRunOutput {
    pub command: String,
    pub component_id: String,
    pub output_dir: String,
    pub applied: bool,
    pub commands: Vec<ReconcileRunCommandOutput>,
    pub totals: ReconcileRunTotals,
}

#[derive(Serialize)]
pub struct ReconcileRunCommandOutput {
    pub command: String,
    pub component_id: String,
    pub source: String,
    pub status: ReconcileRunCommandStatus,
    pub warnings: Vec<String>,
    #[serde(flatten)]
    pub issue_totals: ReconcileRunIssueTotals,
    pub failures: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconcile: Option<ReconcileOutput>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileRunCommandStatus {
    Processed,
    SkippedMissingOutput,
    SkippedMalformedOutput,
    Failed,
}

#[derive(Default, Serialize)]
pub struct ReconcileRunTotals {
    pub commands_processed: usize,
    #[serde(flatten)]
    pub issue_totals: ReconcileRunIssueTotals,
    pub failures: usize,
}

#[derive(Clone, Copy, Default, Serialize)]
pub struct ReconcileRunIssueTotals {
    pub issues_created: usize,
    pub issues_updated: usize,
    pub issues_closed: usize,
}

struct ReconcileCommandRequest {
    component_id: String,
    findings: Option<String>,
    from_output: Vec<(String, String)>,
    run_url: Option<String>,
    no_refresh_closed: bool,
    list_limit: usize,
    apply: bool,
    path: Option<String>,
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
            let (output, exit) = run_reconcile_command(ReconcileCommandRequest {
                component_id,
                findings,
                from_output,
                run_url,
                no_refresh_closed,
                list_limit,
                apply,
                path,
            })?;
            Ok((IssuesCommandOutput::Reconcile(output), exit))
        }
        IssuesCommand::ReconcileRun {
            component_id,
            output_dir,
            commands,
            run_url,
            no_refresh_closed,
            list_limit,
            apply,
            path,
        } => {
            let output = run_reconcile_run(
                component_id,
                output_dir,
                commands,
                run_url,
                no_refresh_closed,
                list_limit,
                apply,
                path,
            )?;
            let exit = if output.totals.failures > 0 { 1 } else { 0 };
            Ok((IssuesCommandOutput::ReconcileRun(output), exit))
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

fn run_reconcile_command(
    request: ReconcileCommandRequest,
) -> homeboy::core::Result<(ReconcileOutput, i32)> {
    let findings_input = read_reconcile_input(
        request.findings.as_deref(),
        &request.from_output,
        request.run_url,
    )?;
    let command_label = findings_input.command.clone();
    let groups = into_issue_groups(findings_input, &request.component_id);

    let config = build_reconcile_config(request.no_refresh_closed);

    // Default tracker = GitHub against the component's remote.
    let tracker_impl = GithubTracker::new(request.component_id.clone()).with_path(request.path);

    // Fetch existing issues for label-scoping.
    let existing = tracker_impl.list_issues(&command_label, request.list_limit)?;

    // Pure decision.
    let plan = reconcile_scoped(
        &groups,
        &existing,
        &config,
        &command_label,
        &request.component_id,
    );
    let plan_lines = render_plan_lines(&plan);
    let plan_summary = summarize_plan(&plan);

    if request.apply {
        let result = apply_plan(plan, &tracker_impl)?;
        let exit = if result.failed_count > 0 { 1 } else { 0 };
        let output = ReconcileOutput {
            component_id: request.component_id,
            command: command_label,
            applied: true,
            plan_summary,
            result: Some(result),
            plan_lines,
        };
        Ok((output, exit))
    } else {
        let output = ReconcileOutput {
            component_id: request.component_id,
            command: command_label,
            applied: false,
            plan_summary,
            result: None,
            plan_lines,
        };
        Ok((output, 0))
    }
}

#[allow(clippy::too_many_arguments)]
fn run_reconcile_run(
    component_id: String,
    output_dir: Option<String>,
    commands: Vec<String>,
    run_url: Option<String>,
    no_refresh_closed: bool,
    list_limit: usize,
    apply: bool,
    path: Option<String>,
) -> homeboy::core::Result<ReconcileRunOutput> {
    let output_dir = discover_output_dir(output_dir)?;
    let commands = normalize_reconcile_run_commands(commands);
    let mut command_outputs = Vec::new();
    let mut totals = ReconcileRunTotals::default();

    for command in commands {
        let source = output_dir.join(format!("{command}.json"));
        let source_display = source.display().to_string();

        match inspect_reconcile_run_output(&source) {
            OutputInspection::Missing(reason) => {
                command_outputs.push(ReconcileRunCommandOutput {
                    command,
                    component_id: component_id.clone(),
                    source: source_display,
                    status: ReconcileRunCommandStatus::SkippedMissingOutput,
                    warnings: vec![reason],
                    issue_totals: ReconcileRunIssueTotals::default(),
                    failures: 0,
                    reconcile: None,
                });
            }
            OutputInspection::Malformed(reason) => {
                totals.failures += 1;
                command_outputs.push(ReconcileRunCommandOutput {
                    command,
                    component_id: component_id.clone(),
                    source: source_display,
                    status: ReconcileRunCommandStatus::SkippedMalformedOutput,
                    warnings: vec![reason],
                    issue_totals: ReconcileRunIssueTotals::default(),
                    failures: 1,
                    reconcile: None,
                });
            }
            OutputInspection::Valid(value) => {
                let command_component_id =
                    component_id_from_native_output(&value).unwrap_or_else(|| component_id.clone());
                let result = run_reconcile_command(ReconcileCommandRequest {
                    component_id: command_component_id.clone(),
                    findings: None,
                    from_output: vec![(command.clone(), source_display.clone())],
                    run_url: run_url.clone(),
                    no_refresh_closed,
                    list_limit,
                    apply,
                    path: path.clone(),
                });

                match result {
                    Ok((reconcile, exit)) => {
                        let aggregate = aggregate_reconcile_output(&reconcile);
                        totals.commands_processed += 1;
                        totals.issue_totals.issues_created += aggregate.0.issues_created;
                        totals.issue_totals.issues_updated += aggregate.0.issues_updated;
                        totals.issue_totals.issues_closed += aggregate.0.issues_closed;
                        totals.failures += aggregate.1;
                        if exit != 0 && aggregate.1 == 0 {
                            totals.failures += 1;
                        }
                        command_outputs.push(ReconcileRunCommandOutput {
                            command,
                            component_id: command_component_id,
                            source: source_display,
                            status: ReconcileRunCommandStatus::Processed,
                            warnings: Vec::new(),
                            issue_totals: aggregate.0,
                            failures: aggregate.1,
                            reconcile: Some(reconcile),
                        });
                    }
                    Err(err) => {
                        totals.failures += 1;
                        command_outputs.push(ReconcileRunCommandOutput {
                            command,
                            component_id: command_component_id,
                            source: source_display,
                            status: ReconcileRunCommandStatus::Failed,
                            warnings: vec![err.to_string()],
                            issue_totals: ReconcileRunIssueTotals::default(),
                            failures: 1,
                            reconcile: None,
                        });
                    }
                }
            }
        }
    }

    Ok(ReconcileRunOutput {
        command: "issues.reconcile-run".to_string(),
        component_id,
        output_dir: output_dir.display().to_string(),
        applied: apply,
        commands: command_outputs,
        totals,
    })
}

// ---------------------------------------------------------------------------
// Reconcile-run helpers
// ---------------------------------------------------------------------------

enum OutputInspection {
    Missing(String),
    Malformed(String),
    Valid(Value),
}

fn discover_output_dir(output_dir: Option<String>) -> homeboy::core::Result<PathBuf> {
    match output_dir.or_else(|| std::env::var("HOMEBOY_OUTPUT_DIR").ok()) {
        Some(dir) if !dir.trim().is_empty() => Ok(PathBuf::from(dir)),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "output-dir",
            "Missing --output-dir and HOMEBOY_OUTPUT_DIR is not set",
            None,
            Some(vec![
                "Pass --output-dir <dir>".to_string(),
                "Set HOMEBOY_OUTPUT_DIR to the structured output directory".to_string(),
            ]),
        )),
    }
}

fn normalize_reconcile_run_commands(commands: Vec<String>) -> Vec<String> {
    commands
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|command| matches!(command.as_str(), "audit" | "lint" | "test"))
        .collect()
}

fn inspect_reconcile_run_output(path: &Path) -> OutputInspection {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return OutputInspection::Missing(format!(
                "No structured output found at {}",
                path.display()
            ))
        }
    };

    if metadata.len() == 0 {
        return OutputInspection::Missing(format!(
            "Structured output is empty at {}",
            path.display()
        ));
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            return OutputInspection::Malformed(format!(
                "Could not read structured output at {}: {}",
                path.display(),
                err
            ))
        }
    };

    match serde_json::from_str(&raw) {
        Ok(value) => OutputInspection::Valid(value),
        Err(err) => OutputInspection::Malformed(format!(
            "Structured output is malformed at {}: {}",
            path.display(),
            err
        )),
    }
}

fn component_id_from_native_output(value: &Value) -> Option<String> {
    value
        .pointer("/data/component_id")
        .or_else(|| value.pointer("/data/component"))
        .or_else(|| value.get("component_id"))
        .or_else(|| value.get("component"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

fn aggregate_reconcile_output(output: &ReconcileOutput) -> (ReconcileRunIssueTotals, usize) {
    if let Some(result) = &output.result {
        let mut issue_totals = ReconcileRunIssueTotals::default();
        let mut failures = 0;
        for execution in &result.executions {
            match execution.outcome {
                homeboy::core::issues::apply::ExecutionOutcome::Filed { .. } => {
                    issue_totals.issues_created += 1;
                }
                homeboy::core::issues::apply::ExecutionOutcome::Updated { .. }
                | homeboy::core::issues::apply::ExecutionOutcome::UpdatedClosed { .. } => {
                    issue_totals.issues_updated += 1;
                }
                homeboy::core::issues::apply::ExecutionOutcome::Closed { .. }
                | homeboy::core::issues::apply::ExecutionOutcome::ClosedDuplicate { .. } => {
                    issue_totals.issues_closed += 1;
                }
                homeboy::core::issues::apply::ExecutionOutcome::Failed { .. } => {
                    failures += 1;
                }
                homeboy::core::issues::apply::ExecutionOutcome::Skipped => {}
            }
        }
        (issue_totals, failures)
    } else {
        (
            ReconcileRunIssueTotals {
                issues_created: output.plan_summary.file_new,
                issues_updated: output.plan_summary.update + output.plan_summary.update_closed,
                issues_closed: output.plan_summary.close + output.plan_summary.close_duplicate,
            },
            0,
        )
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
    use homeboy::core::issues::apply::{ExecutionOutcome, ReconcileExecution};
    use homeboy::core::issues::{ReconcilePlan, ReconcileResult};
    use serde_json::json;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    #[test]
    fn reconcile_run_discovers_output_dir_from_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old = std::env::var("HOMEBOY_OUTPUT_DIR").ok();
        std::env::set_var("HOMEBOY_OUTPUT_DIR", "/tmp/homeboy-output");

        let discovered = discover_output_dir(None).unwrap();

        assert_eq!(discovered, PathBuf::from("/tmp/homeboy-output"));
        match old {
            Some(value) => std::env::set_var("HOMEBOY_OUTPUT_DIR", value),
            None => std::env::remove_var("HOMEBOY_OUTPUT_DIR"),
        }
    }

    #[test]
    fn reconcile_run_reports_missing_and_malformed_outputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("audit.json");
        let malformed = dir.path().join("lint.json");
        std::fs::write(&malformed, "not json").expect("write malformed output");

        let missing_result = inspect_reconcile_run_output(&missing);
        let malformed_result = inspect_reconcile_run_output(&malformed);

        assert!(matches!(missing_result, OutputInspection::Missing(_)));
        assert!(matches!(malformed_result, OutputInspection::Malformed(_)));
    }

    #[test]
    fn reconcile_run_aggregates_applied_execution_totals() {
        let output = ReconcileOutput {
            component_id: "homeboy".to_string(),
            command: "audit".to_string(),
            applied: true,
            plan_summary: ReconcileOutputSummary::default(),
            plan_lines: Vec::new(),
            result: Some(ReconcileResult {
                plan: ReconcilePlan::new("homeboy", Vec::new()),
                executions: vec![
                    ReconcileExecution {
                        summary: "file".to_string(),
                        outcome: ExecutionOutcome::Filed { number: 1 },
                    },
                    ReconcileExecution {
                        summary: "update".to_string(),
                        outcome: ExecutionOutcome::Updated { number: 2 },
                    },
                    ReconcileExecution {
                        summary: "update closed".to_string(),
                        outcome: ExecutionOutcome::UpdatedClosed { number: 3 },
                    },
                    ReconcileExecution {
                        summary: "close".to_string(),
                        outcome: ExecutionOutcome::Closed { number: 4 },
                    },
                    ReconcileExecution {
                        summary: "duplicate".to_string(),
                        outcome: ExecutionOutcome::ClosedDuplicate { number: 5, keep: 4 },
                    },
                    ReconcileExecution {
                        summary: "failed".to_string(),
                        outcome: ExecutionOutcome::Failed {
                            error: "boom".to_string(),
                        },
                    },
                ],
                counts: Default::default(),
                failed_count: 1,
            }),
        };

        let aggregate = aggregate_reconcile_output(&output);

        assert_eq!(aggregate.0.issues_created, 1);
        assert_eq!(aggregate.0.issues_updated, 2);
        assert_eq!(aggregate.0.issues_closed, 2);
        assert_eq!(aggregate.1, 1);
    }

    #[test]
    fn reconcile_run_aggregates_dry_run_plan_totals() {
        let output = ReconcileOutput {
            component_id: "homeboy".to_string(),
            command: "audit".to_string(),
            applied: false,
            plan_summary: ReconcileOutputSummary {
                total_actions: 5,
                file_new: 1,
                update: 1,
                update_closed: 1,
                close: 1,
                close_duplicate: 1,
                skip: 0,
            },
            result: None,
            plan_lines: Vec::new(),
        };

        let aggregate = aggregate_reconcile_output(&output);

        assert_eq!(aggregate.0.issues_created, 1);
        assert_eq!(aggregate.0.issues_updated, 2);
        assert_eq!(aggregate.0.issues_closed, 2);
        assert_eq!(aggregate.1, 0);
    }

    #[test]
    fn reconcile_run_output_serializes_structured_json() {
        let output = ReconcileRunOutput {
            command: "issues.reconcile-run".to_string(),
            component_id: "homeboy".to_string(),
            output_dir: "/tmp/homeboy-output".to_string(),
            applied: true,
            commands: vec![ReconcileRunCommandOutput {
                command: "audit".to_string(),
                component_id: "homeboy".to_string(),
                source: "/tmp/homeboy-output/audit.json".to_string(),
                status: ReconcileRunCommandStatus::Processed,
                warnings: Vec::new(),
                issue_totals: ReconcileRunIssueTotals {
                    issues_created: 1,
                    issues_updated: 2,
                    issues_closed: 3,
                },
                failures: 0,
                reconcile: None,
            }],
            totals: ReconcileRunTotals {
                commands_processed: 1,
                issue_totals: ReconcileRunIssueTotals {
                    issues_created: 1,
                    issues_updated: 2,
                    issues_closed: 3,
                },
                failures: 0,
            },
        };

        let value = serde_json::to_value(output).unwrap();

        assert_eq!(
            value,
            json!({
                "command": "issues.reconcile-run",
                "component_id": "homeboy",
                "output_dir": "/tmp/homeboy-output",
                "applied": true,
                "commands": [{
                    "command": "audit",
                    "component_id": "homeboy",
                    "source": "/tmp/homeboy-output/audit.json",
                    "status": "processed",
                    "warnings": [],
                    "issues_created": 1,
                    "issues_updated": 2,
                    "issues_closed": 3,
                    "failures": 0
                }],
                "totals": {
                    "commands_processed": 1,
                    "issues_created": 1,
                    "issues_updated": 2,
                    "issues_closed": 3,
                    "failures": 0
                }
            })
        );
    }
}
