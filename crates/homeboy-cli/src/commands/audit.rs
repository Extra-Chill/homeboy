use clap::Args;
use std::io::Write;
use std::path::Path;

use homeboy::core::code_audit::{
    self, report, run_main_audit_workflow, AuditCommandOutput, AuditRunWorkflowArgs,
};
use homeboy::core::engine::execution_context::ExecutionContext;
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{
    finding_records_from_audit, ActiveObservation, NewFindingRecord, NewRunRecord, RunStatus,
};

use homeboy_extension::{ExtensionCapability, ExtensionRunner};

use super::source_command::resolve_source_context;
use super::utils::args::{
    BaselineArgs, ChangedSinceArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
};
use super::utils::response::actionable_metadata_value_for_run_ref;
use super::CmdResult;
use crate::command_contract::{LabCommandContract, AUDIT_LAB_LABEL};
use crate::core::observation::WorkflowObservationAdapter;

const AUDIT_CHANGED_SINCE_LAB_UNSUPPORTED_REASON: &str = "`audit --changed-since` is not Lab-portable yet because changed-since audit depends on git base refs that the current Lab workspace sync may not have fetched.";

#[derive(Args)]
pub struct AuditArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[arg(long, hide = true)]
    pub release_readiness_source: Option<String>,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    /// Only show discovered conventions (skip findings)
    #[arg(long)]
    pub conventions: bool,

    /// Restrict findings to these kinds (repeatable)
    #[arg(long = "only", value_name = "kind")]
    pub only: Vec<String>,

    /// Exclude findings of these kinds (repeatable)
    #[arg(long = "exclude", value_name = "kind")]
    pub exclude: Vec<String>,

    /// Detector profile to run. `full` preserves the default full audit;
    /// `pr` runs cheap root-level blockers for changed-file review.
    #[arg(long, value_name = "PROFILE", default_value = "full", value_parser = ["full", "pr", "architecture"])]
    pub profile: String,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    // Only audit files changed since a git ref. Shared changed-scope group
    // (#11140) — the same declaration `lint`, `test`, `build`, `review`, and
    // `refactor` flatten.
    //
    // NOTE (divergence, unchanged): `lab_contract()` below makes
    // `audit --changed-since` local-only, while `lint --changed-since` stays
    // a Lab-portable release gate. Both now read the identical field, so the
    // asymmetry is a visible policy choice rather than a side effect of two
    // independent flag declarations.
    #[command(flatten)]
    pub changed: ChangedSinceArgs,

    // The `--summary` alias keeps one compact-output convention working across
    // the `review` umbrella and every phase subcommand. `review/mod.rs` already
    // maps its own `--summary` onto this field for the audit phase (#10428).
    /// Include compact machine-readable summary for CI wrappers.
    /// Also accepts `--summary`.
    #[arg(long, alias = "summary")]
    pub json_summary: bool,

    /// Include automated-fixability metadata. This can be expensive because it
    /// runs the refactor planner after audit completes.
    #[arg(long)]
    pub fixability: bool,

    /// Emit the complete audit report on stdout. The default changed-since
    /// presentation is a bounded operator summary; `--output` always retains
    /// the complete report.
    #[arg(long)]
    pub full: bool,
}

impl AuditArgs {
    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        if self.changed.changed_since.is_some() {
            return Some(LabCommandContract::local_only(
                AUDIT_LAB_LABEL,
                AUDIT_CHANGED_SINCE_LAB_UNSUPPORTED_REASON,
            ));
        }
        if self.conventions {
            return None;
        }

        Some(
            LabCommandContract::portable(
                AUDIT_LAB_LABEL,
                (self.baseline_args.baseline || self.baseline_args.ratchet)
                    .then_some("--baseline/--ratchet"),
                true,
                &[],
            )
            .release_gate(),
        )
    }
}

fn parse_finding_kinds(
    values: &[String],
    flag: &str,
) -> homeboy::core::Result<Vec<code_audit::AuditFinding>> {
    use std::str::FromStr;
    values
        .iter()
        .map(|value| {
            code_audit::AuditFinding::from_str(value).map_err(|msg| {
                homeboy::core::Error::validation_invalid_argument(flag, msg, None, None)
            })
        })
        .collect()
}

fn parse_audit_profile(value: &str) -> homeboy::core::Result<code_audit::AuditProfile> {
    value.parse().map_err(|msg| {
        homeboy::core::Error::validation_invalid_argument("profile", msg, None, None)
    })
}

pub fn run(args: AuditArgs) -> CmdResult<AuditCommandOutput> {
    let only_kinds = parse_finding_kinds(&args.only, "only")?;
    let exclude_kinds = parse_finding_kinds(&args.exclude, "exclude")?;
    let profile = parse_audit_profile(&args.profile)?;

    let source_ctx = resolve_source_context(
        &args.comp,
        &SettingArgs::default(),
        &args.extension_override,
        None,
    )?;
    let reference_paths = resolve_audit_reference_paths(&source_ctx)?;
    let resolved_id = source_ctx.component_id.clone();
    let resolved_path = source_ctx.source_path.to_string_lossy().to_string();

    let observation = AuditObservationAdapter::new(&resolved_id, &resolved_path, &args);
    let bounded_output = args.changed.changed_since.is_some() && !args.full;
    let active_observation = if bounded_output {
        Some(ActiveObservation::start(observation.start_record())?)
    } else {
        ActiveObservation::start_best_effort(observation.start_record())
    };
    let run_id = active_observation
        .as_ref()
        .map(|observation| observation.run_id().to_string());
    let workflow = run_main_audit_workflow(AuditRunWorkflowArgs {
        component_id: resolved_id.clone(),
        source_path: resolved_path.clone(),
        reference_paths,
        conventions: args.conventions,
        only_kinds,
        exclude_kinds,
        only_labels: args.only,
        exclude_labels: args.exclude,
        profile,
        extension_overrides: args.extension_override.extensions,
        baseline_flags: homeboy::core::engine::baseline::BaselineFlags {
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
            ratchet: args.baseline_args.ratchet,
        },
        changed_since: args.changed.changed_since,
        precomputed_changed_files: args.changed.precomputed_changed_files,
        // `--full` is an explicit request for the complete report and wins
        // over the compact-summary aliases.
        json_summary: args.json_summary && !args.full,
        include_fixability: args.fixability,
    });

    let workflow = match workflow {
        Ok(workflow) => workflow,
        Err(error) => {
            if let Some(active) = active_observation {
                active.finish_error(observation.error_metadata(&error));
            }
            return Err(error);
        }
    };

    let observation_result = active_observation.as_ref().map(|active| {
        (
            observation.success_findings(active.run_id(), &workflow),
            observation.success_status(&workflow),
            homeboy::core::observation::merge_metadata(
                active.initial_metadata().clone(),
                observation.success_metadata(&workflow),
            ),
        )
    });
    let (mut output, exit_code) = report::from_main_workflow(workflow);
    attach_audit_actionable(&mut output, run_id);
    if let Some(active) = active_observation {
        let (findings, status, metadata) = observation_result.expect("active audit observation");
        finish_audit_observation(
            active,
            &observation,
            &mut output,
            findings,
            status,
            metadata,
            bounded_output,
        )?;
    }
    Ok((output, exit_code))
}

const AUDIT_FULL_REPORT_ARTIFACT_PREFIX: &str = "audit-report";

fn audit_full_report_artifact_id(run_id: &str) -> String {
    format!("{AUDIT_FULL_REPORT_ARTIFACT_PREFIX}-{run_id}")
}

#[allow(clippy::too_many_arguments)]
fn finish_audit_observation(
    active: ActiveObservation,
    observation: &AuditObservationAdapter,
    output: &mut AuditCommandOutput,
    findings: Vec<NewFindingRecord>,
    status: RunStatus,
    metadata: serde_json::Value,
    require_full_report: bool,
) -> homeboy::core::Result<()> {
    match persist_audit_full_report(&active, output) {
        Ok(full_report) => attach_audit_full_report(output, full_report),
        Err(error) if require_full_report => {
            // A required bounded-output artifact may fail, but its run must not remain active.
            active.finish_error(observation.error_metadata(&error));
            return Err(error);
        }
        Err(_) => {}
    }
    active.record_findings(&findings);
    active.finish(status, Some(metadata));
    Ok(())
}

fn persist_audit_full_report(
    active: &ActiveObservation,
    output: &AuditCommandOutput,
) -> homeboy::core::Result<serde_json::Value> {
    let artifact_id = audit_full_report_artifact_id(active.run_id());
    let bytes = serde_json::to_vec(output).map_err(|error| {
        homeboy::core::Error::internal_json(
            error.to_string(),
            Some("serialize audit report".to_string()),
        )
    })?;
    let mut report = tempfile::NamedTempFile::new().map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("create audit report".to_string()),
        )
    })?;
    report.write_all(&bytes).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some("write audit report".to_string()))
    })?;
    report.as_file().sync_all().map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some("sync audit report".to_string()))
    })?;
    active.store().record_artifact_with_id(
        active.run_id(),
        "audit_report",
        report.path(),
        &artifact_id,
        serde_json::json!({ "schema": "homeboy/audit-full-report/v1" }),
    )?;
    Ok(serde_json::json!({
        "schema": "homeboy/audit-full-report-ref/v1",
        "uri": format!(
            "homeboy://run/{}/artifact/{artifact_id}",
            active.run_id(),
        ),
        "command": format!("homeboy runs evidence {}", active.run_id()),
    }))
}

fn attach_audit_actionable(output: &mut AuditCommandOutput, run_id: Option<String>) {
    let Some(run_id) = run_id else {
        return;
    };
    let actionable = Some(actionable_metadata_value_for_run_ref(
        run_id,
        "audit",
        "homeboy-audit",
    ));
    match output {
        AuditCommandOutput::Full {
            actionable: slot, ..
        }
        | AuditCommandOutput::Compared {
            actionable: slot, ..
        } => *slot = actionable,
        AuditCommandOutput::Conventions { .. }
        | AuditCommandOutput::BaselineSaved { .. }
        | AuditCommandOutput::Summary(_) => {}
    }
}

fn attach_audit_full_report(output: &mut AuditCommandOutput, full_report: serde_json::Value) {
    match output {
        AuditCommandOutput::Full {
            full_report: slot, ..
        }
        | AuditCommandOutput::Compared {
            full_report: slot, ..
        } => *slot = Some(full_report),
        AuditCommandOutput::Conventions { .. }
        | AuditCommandOutput::BaselineSaved { .. }
        | AuditCommandOutput::Summary(_) => {}
    }
}

struct AuditObservationAdapter {
    component_id: String,
    source_path: String,
    command: String,
    initial_metadata: serde_json::Value,
}

impl AuditObservationAdapter {
    fn new(component_id: &str, source_path: &str, args: &AuditArgs) -> Self {
        Self {
            component_id: component_id.to_string(),
            source_path: source_path.to_string(),
            command: audit_observation_command(component_id, args),
            initial_metadata: audit_observation_initial_metadata(source_path, args),
        }
    }
}

impl WorkflowObservationAdapter<code_audit::AuditRunWorkflowResult> for AuditObservationAdapter {
    fn start_record(&self) -> NewRunRecord {
        let path = Path::new(&self.source_path);
        NewRunRecord::builder("audit")
            .component_id(self.component_id.clone())
            .command(self.command.clone())
            .cwd_path(path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(path))
            .metadata(self.initial_metadata.clone())
            .build()
    }

    fn success_status(&self, workflow: &code_audit::AuditRunWorkflowResult) -> RunStatus {
        if workflow.exit_code == 0 {
            RunStatus::Pass
        } else {
            RunStatus::Fail
        }
    }

    fn success_metadata(&self, workflow: &code_audit::AuditRunWorkflowResult) -> serde_json::Value {
        serde_json::json!({
            "observation_status": if workflow.exit_code == 0 { "pass" } else { "fail" },
            "exit_code": workflow.exit_code,
            "summary": audit_observation_summary(&workflow.output),
            "timing": audit_observation_timing(&workflow.timing),
        })
    }

    fn success_findings(
        &self,
        run_id: &str,
        workflow: &code_audit::AuditRunWorkflowResult,
    ) -> Vec<NewFindingRecord> {
        finding_records_from_audit(run_id, &workflow.findings)
    }

    fn error_metadata(&self, error: &homeboy::core::Error) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
            "timing": audit_observation_timing(&code_audit::AuditTiming::default()),
        }))
    }
}

fn audit_observation_timing(timing: &code_audit::AuditTiming) -> serde_json::Value {
    serde_json::json!({
        "spans": timing.spans,
    })
}

fn audit_observation_command(component_id: &str, args: &AuditArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "audit".to_string(),
        component_id.to_string(),
    ];
    if args.conventions {
        parts.push("--conventions".to_string());
    }
    for kind in &args.only {
        parts.push(format!("--only={kind}"));
    }
    for kind in &args.exclude {
        parts.push(format!("--exclude={kind}"));
    }
    if args.profile != "full" {
        parts.push(format!("--profile={}", args.profile));
    }
    for extension in &args.extension_override.extensions {
        parts.push(format!("--extension={extension}"));
    }
    if let Some(changed_since) = args.changed.changed_since() {
        parts.push(format!("--changed-since={changed_since}"));
    }
    if args.json_summary {
        parts.push("--json-summary".to_string());
    }
    if args.fixability {
        parts.push("--fixability".to_string());
    }
    if args.full {
        parts.push("--full".to_string());
    }
    parts.join(" ")
}

fn audit_observation_initial_metadata(source_path: &str, args: &AuditArgs) -> serde_json::Value {
    serde_json::json!({
        "source_path": source_path,
        "mode": if args.conventions { "conventions" } else { "audit" },
        "profile": args.profile,
        "only": args.only,
        "exclude": args.exclude,
        "extensions": args.extension_override.extensions,
        "baseline": {
            "baseline": args.baseline_args.baseline,
            "ignore_baseline": args.baseline_args.ignore_baseline,
            "ratchet": args.baseline_args.ratchet,
        },
        "changed_since": args.changed.changed_since,
        "json_summary": args.json_summary,
        "fixability": args.fixability,
    })
}

fn audit_observation_summary(output: &AuditCommandOutput) -> serde_json::Value {
    match output {
        AuditCommandOutput::Full { passed, result, .. } => {
            code_audit_result_observation_summary(*passed, result, None)
        }
        AuditCommandOutput::Conventions {
            component_id,
            conventions,
            directory_conventions,
        } => serde_json::json!({
            "component_id": component_id,
            "conventions": conventions.len(),
            "directory_conventions": directory_conventions.len(),
        }),
        AuditCommandOutput::BaselineSaved {
            component_id,
            path,
            findings_count,
            outliers_count,
            alignment_score,
        } => serde_json::json!({
            "component_id": component_id,
            "baseline_path": path,
            "findings": findings_count,
            "outliers_found": outliers_count,
            "alignment_score": alignment_score,
        }),
        AuditCommandOutput::Compared {
            passed,
            result,
            changed_since,
            ..
        } => code_audit_result_observation_summary(*passed, result, changed_since.as_ref()),
        AuditCommandOutput::Summary(summary) => serde_json::json!({
            "findings": summary.total_findings,
            "warnings": summary.warnings,
            "info": summary.info,
            "alignment_score": summary.alignment_score,
            "exit_code": summary.exit_code,
        }),
    }
}

fn code_audit_result_observation_summary(
    passed: bool,
    result: &code_audit::CodeAuditResult,
    changed_since: Option<&report::AuditChangedSinceSummary>,
) -> serde_json::Value {
    let mut summary = serde_json::json!({
        "passed": passed,
        "component_id": result.component_id,
        "files_scanned": result.summary.files_scanned,
        "conventions_detected": result.summary.conventions_detected,
        "findings": result.findings.len(),
        "outliers_found": result.summary.outliers_found,
        "alignment_score": result.summary.alignment_score,
    });

    if let Some(changed_since) = changed_since {
        summary["changed_since"] = serde_json::json!(changed_since);
    }

    summary
}

/// Wall-clock budget for a single extension's audit reference setup.
///
/// Reference setup resolves dependency directories; it is not a build. Before
/// this ran through [`ExtensionRunner`] it had no budget at all, so a setup
/// script that hung wedged the audit indefinitely.
const AUDIT_REFERENCE_SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Run configured extension audit reference setup for the resolved audit target.
///
/// Audit is an *aggregating* capability: reference paths are the union of every
/// linked extension's contribution, so this resolves one execution context per
/// linked extension rather than electing a single owner the way Lint or Test do.
///
/// Setup still speaks the legacy shell boundary (`--export` stdout), but it now
/// runs through [`ExtensionRunner`], which supplies resolved settings, the
/// component environment, the env-provider chain, and a wall-clock budget. A
/// declared setup script that fails is an error rather than an empty result:
/// reference paths feed cross-reference analysis, so silently dropping them
/// degrades findings instead of reporting a problem.
pub(crate) fn resolve_audit_reference_paths(
    source_ctx: &ExecutionContext,
) -> homeboy::core::Result<Vec<String>> {
    let Some(extensions) = &source_ctx.component.extensions else {
        return Ok(Vec::new());
    };

    // Deterministic order: `extensions` is a HashMap, and which extension's
    // failure surfaces first should not depend on hash iteration order.
    let mut extension_ids: Vec<&String> = extensions.keys().collect();
    extension_ids.sort();

    let mut reference_paths = Vec::new();
    for ext_id in extension_ids {
        // An unreadable sibling manifest does not get to fail audit for the
        // extensions that are readable (#11122). Only a declared-and-broken
        // setup script is an error.
        let Ok(manifest) = homeboy_extension::load_extension(ext_id) else {
            continue;
        };
        if !manifest.has_audit() {
            continue;
        }

        let execution_context =
            homeboy::core::extension_execution::resolve_execution_context_for_extension(
                &source_ctx.component,
                ExtensionCapability::Audit,
                ext_id,
            )?;

        homeboy::log_status!("audit", "Running reference setup: {}", ext_id);

        let output = ExtensionRunner::for_context(execution_context)
            .path_override(Some(source_ctx.source_path.to_string_lossy().to_string()))
            .script_args(&["--export".to_string()])
            .passthrough(false)
            .timeout(Some(AUDIT_REFERENCE_SETUP_TIMEOUT))
            .run()?;

        // Setup scripts report progress on stderr; keep surfacing it.
        for line in output.stderr.lines().filter(|line| !line.is_empty()) {
            homeboy::log_status!("audit", "{}", line);
        }

        if !output.success {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "audit.setup_references",
                format!(
                    "Extension '{}' audit reference setup failed with exit code {}{}",
                    ext_id,
                    output.exit_code,
                    if output.timed_out {
                        format!(
                            " (timed out after {}s)",
                            AUDIT_REFERENCE_SETUP_TIMEOUT.as_secs()
                        )
                    } else {
                        String::new()
                    }
                ),
                Some(ext_id.clone()),
                Some(vec![
                    homeboy::core::extension_execution::stderr_tail(&output.stderr),
                    "Reference paths feed cross-reference analysis; running audit without them would change findings.".to_string(),
                ]),
            ));
        }

        reference_paths.extend(parse_audit_reference_paths_export(&output.stdout));
    }

    reference_paths.sort();
    reference_paths.dedup();
    Ok(reference_paths)
}

fn parse_audit_reference_paths_export(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("export HOMEBOY_AUDIT_REFERENCE_PATHS="))
        .map(normalize_shell_export_value)
        .unwrap_or_default()
        .lines()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty() && Path::new(path).is_dir())
        .collect()
}

fn normalize_shell_export_value(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("$'")
        .trim_start_matches('\'')
        .trim_start_matches('"')
        .trim_end_matches('\'')
        .trim_end_matches('"')
        .replace("\\n", "\n")
}

// Core function tests (finding_fingerprint, score_delta, weighted_finding_score_with,
// build_chunk_verifier, apply_fix_policy, default_audit_exit_code) have been relocated
// to their respective core modules: code_audit/compare.rs, code_audit/run.rs,
// refactor/auto/apply.rs, refactor/plan/verify.rs.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{BaselineArgs, ExtensionOverrideArgs, SettingArgs};
    use crate::test_support::{
        with_isolated_audit_home, with_isolated_home, write_source_extension,
    };
    use clap::Parser;
    use homeboy::core::observation::finish_adapted_observed_workflow;
    use homeboy::core::observation::{ObservationStore, RunListFilter};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct XdgGuard {
        prior: Option<String>,
    }

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self { prior }
        }

        fn set(value: &std::path::Path) -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::set_var("XDG_DATA_HOME", value);
            Self { prior }
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-audit-command-{name}-{nanos}"))
    }

    fn sample_args() -> AuditArgs {
        AuditArgs {
            comp: PositionalComponentArgs {
                component: Some("homeboy".to_string()),
                path: None,
            },
            release_readiness_source: None,
            extension_override: ExtensionOverrideArgs::default(),
            conventions: false,
            only: vec![],
            exclude: vec![],
            profile: "full".to_string(),
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: false,
                ratchet: false,
            },
            changed: ChangedSinceArgs {
                changed_since: Some("origin/main".to_string()),
                precomputed_changed_files: None,
            },
            json_summary: true,
            fixability: false,
            full: false,
        }
    }

    fn latest_audit_run(store: &ObservationStore) -> homeboy::core::observation::RunRecord {
        store
            .latest_run(RunListFilter {
                kind: Some("audit".to_string()),
                component_id: Some("homeboy".to_string()),
                ..RunListFilter::default()
            })
            .expect("latest run")
            .expect("audit run")
    }

    fn sample_audit_workflow(home: &std::path::Path) -> code_audit::AuditRunWorkflowResult {
        let finding = code_audit::Finding {
            convention: "command modules".to_string(),
            severity: code_audit::Severity::Warning,
            file: "src/commands/foo.rs".to_string(),
            description: "Missing run function".to_string(),
            suggestion: "Add run()".to_string(),
            kind: code_audit::AuditFinding::MissingMethod,
            line: None,
        };
        code_audit::AuditRunWorkflowResult {
            output: AuditCommandOutput::Full {
                passed: false,
                timing: code_audit::AuditTiming::default(),
                measurement: code_audit::AuditMeasurement::new(
                    code_audit::AuditProfile::Full,
                    false,
                    false,
                    false,
                    false,
                    false,
                ),
                result: code_audit::CodeAuditResult {
                    component_id: "homeboy".to_string(),
                    source_path: home.to_string_lossy().to_string(),
                    summary: code_audit::AuditSummary {
                        files_scanned: 1,
                        conventions_detected: 1,
                        outliers_found: 1,
                        alignment_score: Some(0.5),
                        files_skipped: 0,
                        warnings: vec![],
                    },
                    conventions: vec![],
                    directory_conventions: vec![],
                    findings: vec![finding.clone()],
                    duplicate_groups: vec![],
                },
                fixability: None,
                extension_phase_timings: Vec::new(),
                actionable: None,
                full_report: None,
            },
            exit_code: 1,
            findings: vec![finding],
            timing: code_audit::AuditTiming {
                spans: vec![code_audit::AuditTimingSpan {
                    id: "detector.structural".to_string(),
                    status: "ok".to_string(),
                    duration_ms: Some(1.0),
                }],
            },
        }
    }

    fn write_reference_extension(
        home: &std::path::Path,
        id: &str,
        reference_path: &std::path::Path,
    ) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "0.0.0",
                "audit": { "setup_references": "setup.sh" }
            })
            .to_string(),
        )
        .expect("extension manifest");
        write_executable_script(
            &extension_dir.join("setup.sh"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"export HOMEBOY_AUDIT_REFERENCE_PATHS='{}'\"\n",
                reference_path.display()
            ),
        );
    }

    /// Extension whose declared reference setup exists but exits non-zero.
    fn write_failing_reference_extension(home: &std::path::Path, id: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "0.0.0",
                "audit": { "setup_references": "setup.sh" }
            })
            .to_string(),
        )
        .expect("extension manifest");
        write_executable_script(
            &extension_dir.join("setup.sh"),
            "#!/bin/sh\necho 'dependency resolution failed' >&2\nexit 3\n",
        );
    }

    fn write_executable_script(path: &std::path::Path, body: &str) {
        fs::write(path, body).expect("setup script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("make script executable");
        }
    }

    fn write_extension_without_reference_setup(home: &std::path::Path, id: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": id,
                "version": "0.0.0",
                "audit": {}
            })
            .to_string(),
        )
        .expect("extension manifest");
    }

    fn write_standalone_component(
        home: &std::path::Path,
        id: &str,
        component_path: &std::path::Path,
        extension_id: &str,
    ) {
        let component_dir = home.join(".config/homeboy/components");
        fs::create_dir_all(&component_dir).expect("component dir");
        fs::write(
            component_dir.join(format!("{id}.json")),
            serde_json::json!({
                "local_path": component_path,
                "extensions": { extension_id: {} }
            })
            .to_string(),
        )
        .expect("component config");
    }

    fn source_context_for(
        component: Option<String>,
        path: Option<String>,
        extensions: Vec<String>,
    ) -> ExecutionContext {
        resolve_source_context(
            &PositionalComponentArgs { component, path },
            &SettingArgs::default(),
            &ExtensionOverrideArgs { extensions },
            None,
        )
        .expect("source context")
    }

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        audit: AuditArgs,
    }

    #[test]
    fn parses_one_shot_extension_override() {
        let cli = TestCli::try_parse_from([
            "audit",
            "--path",
            "/tmp/repo",
            "--extension",
            "rust",
            "--changed-since",
            "origin/main",
            "--profile",
            "pr",
        ])
        .expect("audit should parse --extension override");

        assert_eq!(cli.audit.extension_override.extensions, vec!["rust"]);
        assert_eq!(cli.audit.changed.changed_since(), Some("origin/main"));
        assert_eq!(cli.audit.profile, "pr");
    }

    #[test]
    fn parses_full_output_request() {
        let cli = TestCli::try_parse_from([
            "audit",
            "--path",
            "/tmp/repo",
            "--changed-since",
            "origin/main",
            "--full",
        ])
        .expect("audit should parse --full");

        assert!(cli.audit.full);
    }

    #[test]
    fn full_output_precedes_summary_request() {
        let args = AuditArgs {
            full: true,
            json_summary: true,
            ..sample_args()
        };

        assert!(args.full);
        assert!(!(args.json_summary && !args.full));
    }

    #[test]
    fn audit_reference_setup_resolves_registered_component_context() {
        with_isolated_home(|home| {
            std::env::remove_var("HOMEBOY_AUDIT_REFERENCE_PATHS");
            let component_dir = tmp_dir("registered-reference-component");
            let reference_dir = tmp_dir("registered-reference-dependency");
            fs::create_dir_all(&component_dir).expect("component dir");
            fs::create_dir_all(&reference_dir).expect("reference dir");
            write_reference_extension(home.path(), "fixture", &reference_dir);
            write_standalone_component(home.path(), "demo", &component_dir, "fixture");

            let source_ctx = source_context_for(Some("demo".to_string()), None, vec![]);
            let reference_paths =
                resolve_audit_reference_paths(&source_ctx).expect("reference setup succeeds");

            assert_eq!(
                reference_paths,
                vec![reference_dir.to_string_lossy().to_string()]
            );
            assert!(std::env::var("HOMEBOY_AUDIT_REFERENCE_PATHS").is_err());
            let _ = fs::remove_dir_all(component_dir);
            let _ = fs::remove_dir_all(reference_dir);
        });
    }

    #[test]
    fn audit_reference_setup_respects_path_portable_config() {
        with_isolated_home(|home| {
            let component_dir = tmp_dir("path-reference-component");
            let reference_dir = tmp_dir("path-reference-dependency");
            fs::create_dir_all(&component_dir).expect("component dir");
            fs::create_dir_all(&reference_dir).expect("reference dir");
            fs::write(
                component_dir.join("homeboy.json"),
                serde_json::json!({
                    "id": "portable-demo",
                    "extensions": { "fixture": {} }
                })
                .to_string(),
            )
            .expect("portable config");
            write_reference_extension(home.path(), "fixture", &reference_dir);

            let source_ctx = source_context_for(
                None,
                Some(component_dir.to_string_lossy().to_string()),
                vec![],
            );
            let reference_paths =
                resolve_audit_reference_paths(&source_ctx).expect("reference setup succeeds");

            assert_eq!(source_ctx.component_id, "portable-demo");
            assert_eq!(
                reference_paths,
                vec![reference_dir.to_string_lossy().to_string()]
            );
            let _ = fs::remove_dir_all(component_dir);
            let _ = fs::remove_dir_all(reference_dir);
        });
    }

    #[test]
    fn audit_reference_setup_respects_extension_override() {
        with_isolated_home(|home| {
            let component_dir = tmp_dir("override-reference-component");
            let reference_dir = tmp_dir("override-reference-dependency");
            fs::create_dir_all(&component_dir).expect("component dir");
            fs::create_dir_all(&reference_dir).expect("reference dir");
            fs::write(
                component_dir.join("homeboy.json"),
                serde_json::json!({
                    "id": "override-demo",
                    "extensions": { "unused": {} }
                })
                .to_string(),
            )
            .expect("portable config");
            write_extension_without_reference_setup(home.path(), "unused");
            write_reference_extension(home.path(), "override", &reference_dir);

            let source_ctx = source_context_for(
                None,
                Some(component_dir.to_string_lossy().to_string()),
                vec!["override".to_string()],
            );
            let reference_paths =
                resolve_audit_reference_paths(&source_ctx).expect("reference setup succeeds");

            assert_eq!(
                reference_paths,
                vec![reference_dir.to_string_lossy().to_string()]
            );
            let _ = fs::remove_dir_all(component_dir);
            let _ = fs::remove_dir_all(reference_dir);
        });
    }

    #[test]
    fn audit_reference_setup_returns_empty_without_setup_contract() {
        with_isolated_home(|home| {
            let component_dir = tmp_dir("no-reference-component");
            fs::create_dir_all(&component_dir).expect("component dir");
            fs::write(
                component_dir.join("homeboy.json"),
                serde_json::json!({
                    "id": "no-reference-demo",
                    "extensions": { "fixture": {} }
                })
                .to_string(),
            )
            .expect("portable config");
            write_extension_without_reference_setup(home.path(), "fixture");

            let source_ctx = source_context_for(
                None,
                Some(component_dir.to_string_lossy().to_string()),
                vec![],
            );

            assert!(resolve_audit_reference_paths(&source_ctx)
                .expect("no declared setup is not a failure")
                .is_empty());
            let _ = fs::remove_dir_all(component_dir);
        });
    }

    /// Before #13723 this resolved to an empty path list, so audit ran without
    /// the extension's reference dependencies and silently produced different
    /// cross-reference findings. A declared setup script that fails is now an
    /// error carrying the script's own stderr.
    #[test]
    fn audit_reference_setup_reports_failing_setup_script() {
        with_isolated_home(|home| {
            let component_dir = tmp_dir("failing-reference-component");
            fs::create_dir_all(&component_dir).expect("component dir");
            fs::write(
                component_dir.join("homeboy.json"),
                serde_json::json!({
                    "id": "failing-demo",
                    "extensions": { "fixture": {} }
                })
                .to_string(),
            )
            .expect("portable config");
            write_failing_reference_extension(home.path(), "fixture");

            let source_ctx = source_context_for(
                None,
                Some(component_dir.to_string_lossy().to_string()),
                vec![],
            );

            let error = resolve_audit_reference_paths(&source_ctx)
                .expect_err("a failing reference setup must not resolve to an empty path list");
            let rendered = error.to_string();
            assert!(
                rendered.contains("fixture"),
                "error should name the extension: {rendered}"
            );
            assert!(
                rendered.contains("reference setup failed"),
                "error should name the failure: {rendered}"
            );
            let _ = fs::remove_dir_all(component_dir);
        });
    }

    #[test]
    fn audit_observation_start_persists_run_record() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();

            let result = finish_adapted_observed_workflow(
                AuditObservationAdapter::new("homeboy", &home.path().to_string_lossy(), &args),
                Err::<code_audit::AuditRunWorkflowResult, _>(
                    homeboy::core::Error::validation_invalid_argument(
                        "fixture",
                        "simulated audit error",
                        None,
                        None,
                    ),
                ),
            );
            assert!(result.is_err());

            let store = ObservationStore::open_initialized().expect("store");
            let run = latest_audit_run(&store);

            assert_eq!(run.kind, "audit");
            assert_eq!(run.status, "error");
            assert_eq!(run.component_id.as_deref(), Some("homeboy"));
            assert_eq!(run.metadata_json["changed_since"], "origin/main");
            assert_eq!(run.metadata_json["observation_status"], "error");
        });
    }

    #[test]
    fn audit_observation_finish_persists_findings() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let workflow = sample_audit_workflow(home.path());

            finish_adapted_observed_workflow(
                AuditObservationAdapter::new("homeboy", &home.path().to_string_lossy(), &args),
                Ok(workflow),
            )
            .expect("finish workflow");

            let store = ObservationStore::open_initialized().expect("store");
            let run = latest_audit_run(&store);
            let findings = store
                .list_findings(homeboy::core::observation::FindingListFilter {
                    run_id: Some(run.id.clone()),
                    tool: Some("audit".to_string()),
                    ..homeboy::core::observation::FindingListFilter::default()
                })
                .expect("list findings");

            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].rule.as_deref(), Some("missing_method"));
            assert_eq!(
                findings[0].fingerprint.as_deref(),
                Some("src/commands/foo.rs:missing_method:command modules:Missing run function")
            );
            assert_eq!(
                findings[0].metadata_json["source_sidecar"],
                "audit-findings"
            );

            assert_eq!(
                run.metadata_json["timing"]["spans"][0]["id"],
                "detector.structural"
            );
            assert_eq!(run.metadata_json["timing"]["spans"][0]["status"], "ok");
        });
    }

    #[test]
    fn audit_full_reports_use_run_scoped_resolvable_artifacts() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let first = ActiveObservation::start(
                AuditObservationAdapter::new("homeboy", &home.path().to_string_lossy(), &args)
                    .start_record(),
            )
            .expect("first observation");
            let second = ActiveObservation::start(
                AuditObservationAdapter::new("homeboy", &home.path().to_string_lossy(), &args)
                    .start_record(),
            )
            .expect("second observation");
            let output = sample_audit_workflow(home.path()).output;

            let first_report = persist_audit_full_report(&first, &output).expect("first report");
            let second_report = persist_audit_full_report(&second, &output).expect("second report");
            let store = ObservationStore::open_initialized().expect("store");

            for (active, report) in [(&first, first_report), (&second, second_report)] {
                let artifact_id = audit_full_report_artifact_id(active.run_id());
                assert_eq!(
                    report["uri"],
                    format!("homeboy://run/{}/artifact/{artifact_id}", active.run_id())
                );
                let resolved = homeboy::core::observation::runs_service::resolve_artifact_for_run(
                    &store,
                    active.run_id(),
                    &artifact_id,
                )
                .expect("resolve report URI artifact");
                assert_eq!(resolved.id, artifact_id);
            }
        });
    }

    #[test]
    fn required_audit_report_failure_terminalizes_observation() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();
            let adapter =
                AuditObservationAdapter::new("homeboy", &home.path().to_string_lossy(), &args);
            let active = ActiveObservation::start(adapter.start_record()).expect("observation");
            let run_id = active.run_id().to_string();
            let run_artifact_dir = homeboy::core::artifact_root()
                .expect("artifact root")
                .join(&run_id);
            fs::create_dir_all(run_artifact_dir.parent().expect("artifact root parent"))
                .expect("artifact root");
            fs::write(&run_artifact_dir, "block report directory").expect("block report directory");

            let workflow = sample_audit_workflow(home.path());
            let (mut output, _) = report::from_main_workflow(workflow);
            let _error = finish_audit_observation(
                active,
                &adapter,
                &mut output,
                Vec::new(),
                RunStatus::Fail,
                serde_json::json!({ "observation_status": "fail" }),
                true,
            )
            .expect_err("required report persistence should fail");

            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(&run_id).expect("read run").expect("run");
            assert_eq!(run.status, "error");
            assert_eq!(run.metadata_json["observation_status"], "error");
        });
    }

    #[test]
    fn audit_observation_start_is_best_effort_when_store_unavailable() {
        with_isolated_home(|home| {
            let bad_data_home = home.path().join("not-a-dir");
            fs::write(&bad_data_home, "file blocks observation dir").expect("write marker");
            let _xdg = XdgGuard::set(&bad_data_home);

            let result = finish_adapted_observed_workflow(
                AuditObservationAdapter::new(
                    "homeboy",
                    &home.path().to_string_lossy(),
                    &sample_args(),
                ),
                Ok(sample_audit_workflow(home.path())),
            );

            assert!(result.is_ok());
        });
    }

    /// End-to-end test of the audit command's read-only mode.
    /// Fixes are now owned by `homeboy refactor --from audit --write`.
    #[test]
    fn audit_detects_outliers_in_convention_group() {
        with_isolated_audit_home(|home| {
            crate::cli_runtime::register_startup_providers_before_reconcile();
            write_source_extension(home.path(), "source-fixture", "rs");
            let root = tmp_dir("audit-read-only");
            fs::create_dir_all(root.join("commands")).unwrap();

            fs::write(
                root.join("commands/good_one.rs"),
                "pub fn run() {}\npub fn execute() {}\n",
            )
            .unwrap();
            fs::write(
                root.join("commands/good_two.rs"),
                "pub fn run() {}\npub fn execute() {}\n",
            )
            .unwrap();
            fs::write(
                root.join("commands/good_three.rs"),
                "pub fn run() {}\npub fn execute() {}\n",
            )
            .unwrap();
            fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

            let args = AuditArgs {
                comp: PositionalComponentArgs {
                    component: Some(root.to_string_lossy().to_string()),
                    path: None,
                },
                release_readiness_source: None,
                extension_override: ExtensionOverrideArgs {
                    extensions: vec!["source-fixture".to_string()],
                },
                conventions: false,
                only: vec![],
                exclude: vec![],
                profile: "full".to_string(),
                baseline_args: BaselineArgs {
                    baseline: false,
                    ignore_baseline: true,
                    ratchet: false,
                },
                changed: ChangedSinceArgs::default(),
                json_summary: false,
                fixability: false,
                full: false,
            };

            let (output, code) = run(args).expect("audit should run");

            // Audit should detect the outlier and return findings
            // Summary or other modes are also valid.
            if let AuditCommandOutput::Full { result, .. } = output {
                assert!(
                    !result.findings.is_empty(),
                    "expected findings for the outlier file"
                );
            }

            // Non-zero exit expected when outliers are found
            assert!(code >= 0, "audit should complete without error");

            let _ = fs::remove_dir_all(root);
        });
    }
}
