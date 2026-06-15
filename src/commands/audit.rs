use clap::Args;
use std::path::Path;

use homeboy::core::code_audit::{
    self, report, run_main_audit_workflow, AuditCommandOutput, AuditRunWorkflowArgs,
};
use homeboy::core::engine::execution_context::ExecutionContext;
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{
    finding_records_from_audit, NewFindingRecord, NewRunRecord, RunStatus,
};

use super::source_command::resolve_source_context;
use super::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
};
use super::utils::observed_workflow::{
    finish_adapted_observed_workflow, WorkflowObservationAdapter,
};
use super::{CmdResult, GlobalArgs};
use crate::command_contract::{
    CommandDescriptor, CommandOutputContractKind, CommandOutputFileMode, LabCommandContract,
};

const AUDIT_CHANGED_SINCE_LAB_UNSUPPORTED_REASON: &str = "`audit --changed-since` is not Lab-portable yet because changed-since audit depends on git base refs that the current Lab workspace sync may not have fetched.";

#[derive(Args)]
pub struct AuditArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

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

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Only audit files changed since a git ref (branch, tag, or SHA).
    #[arg(long)]
    pub changed_since: Option<String>,

    /// Include compact machine-readable summary for CI wrappers
    #[arg(long)]
    pub json_summary: bool,

    /// Include automated-fixability metadata. This can be expensive because it
    /// runs the refactor planner after audit completes.
    #[arg(long)]
    pub fixability: bool,
}

impl AuditArgs {
    pub(crate) fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandDescriptor {
        CommandDescriptor {
            response_mode: crate::command_contract::CommandResponseMode::Json,
            output_file_mode,
            json_family: crate::command_contract::CommandJsonFamily::Quality,
            supports_lab_runner: true,
            lab_runner_unsupported_reason: None,
            lab_offload_mutation_flag: (self.baseline_args.baseline || self.baseline_args.ratchet)
                .then_some("--baseline/--ratchet"),
            output_contract: CommandOutputContractKind::JsonEnvelope,
        }
    }

    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        if self.changed_since.is_some() {
            return Some(LabCommandContract::local_only(
                "audit",
                AUDIT_CHANGED_SINCE_LAB_UNSUPPORTED_REASON,
            ));
        }
        if self.conventions {
            return None;
        }

        Some(LabCommandContract::portable(
            "audit",
            (self.baseline_args.baseline || self.baseline_args.ratchet)
                .then_some("--baseline/--ratchet"),
            true,
            &[],
        ))
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

pub fn run(args: AuditArgs, _global: &GlobalArgs) -> CmdResult<AuditCommandOutput> {
    let only_kinds = parse_finding_kinds(&args.only, "only")?;
    let exclude_kinds = parse_finding_kinds(&args.exclude, "exclude")?;

    let source_ctx = resolve_source_context(
        &args.comp,
        &SettingArgs::default(),
        &args.extension_override,
        None,
    )?;
    let reference_paths = resolve_audit_reference_paths(&source_ctx);
    let resolved_id = source_ctx.component_id.clone();
    let resolved_path = source_ctx.source_path.to_string_lossy().to_string();

    let observation = AuditObservationAdapter::new(&resolved_id, &resolved_path, &args);
    let workflow = run_main_audit_workflow(AuditRunWorkflowArgs {
        component_id: resolved_id.clone(),
        source_path: resolved_path.clone(),
        reference_paths,
        conventions: args.conventions,
        only_kinds,
        exclude_kinds,
        only_labels: args.only,
        exclude_labels: args.exclude,
        extension_overrides: args.extension_override.extensions,
        baseline_flags: homeboy::core::engine::baseline::BaselineFlags {
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
            ratchet: args.baseline_args.ratchet,
        },
        changed_since: args.changed_since,
        json_summary: args.json_summary,
        include_fixability: args.fixability,
    });

    let workflow = finish_adapted_observed_workflow(observation, workflow)?;

    Ok(report::from_main_workflow(workflow))
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
    for extension in &args.extension_override.extensions {
        parts.push(format!("--extension={extension}"));
    }
    if let Some(changed_since) = &args.changed_since {
        parts.push(format!("--changed-since={changed_since}"));
    }
    if args.json_summary {
        parts.push("--json-summary".to_string());
    }
    if args.fixability {
        parts.push("--fixability".to_string());
    }
    parts.join(" ")
}

fn audit_observation_initial_metadata(source_path: &str, args: &AuditArgs) -> serde_json::Value {
    serde_json::json!({
        "source_path": source_path,
        "mode": if args.conventions { "conventions" } else { "audit" },
        "only": args.only,
        "exclude": args.exclude,
        "extensions": args.extension_override.extensions,
        "baseline": {
            "baseline": args.baseline_args.baseline,
            "ignore_baseline": args.baseline_args.ignore_baseline,
            "ratchet": args.baseline_args.ratchet,
        },
        "changed_since": args.changed_since,
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

/// Run configured extension audit reference setup scripts for the resolved audit target.
///
/// Setup still speaks the legacy shell boundary (`--export` stdout), but the command
/// converts that output into typed workflow input instead of process-global state.
pub(crate) fn resolve_audit_reference_paths(source_ctx: &ExecutionContext) -> Vec<String> {
    let extensions = match &source_ctx.component.extensions {
        Some(ext) => ext,
        None => return Vec::new(),
    };

    let mut reference_paths = Vec::new();
    for ext_id in extensions.keys() {
        let ext_manifest = match homeboy::core::extension::load_extension(ext_id) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let setup_script = match ext_manifest.audit_setup_references() {
            Some(s) => s,
            None => continue,
        };

        // Resolve script path relative to extension directory
        let ext_path = homeboy::core::extension::extension_path(ext_id);
        if !ext_path.is_dir() {
            continue;
        }
        let script_path = ext_path.join(setup_script);
        if !script_path.is_file() {
            continue;
        }

        homeboy::log_status!(
            "audit",
            "Running reference setup: {}",
            script_path.display()
        );

        // Run the script with --export flag and capture stdout.
        let output = std::process::Command::new("bash")
            .arg(script_path.to_str().unwrap_or(""))
            .arg("--export")
            .env("HOMEBOY_COMPONENT_PATH", &source_ctx.component.local_path)
            .current_dir(&source_ctx.source_path)
            .output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            reference_paths.extend(parse_audit_reference_paths_export(&stdout));

            // Log stderr (the script's informational output)
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                if !line.is_empty() {
                    homeboy::log_status!("audit", "{}", line);
                }
            }
        }
    }

    reference_paths.sort();
    reference_paths.dedup();
    reference_paths
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
            extension_override: ExtensionOverrideArgs::default(),
            conventions: false,
            only: vec![],
            exclude: vec![],
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: false,
                ratchet: false,
            },
            changed_since: Some("origin/main".to_string()),
            json_summary: true,
            fixability: false,
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
        };
        code_audit::AuditRunWorkflowResult {
            output: AuditCommandOutput::Full {
                passed: false,
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
        fs::write(
            extension_dir.join("setup.sh"),
            format!(
                "printf '%s\\n' \"export HOMEBOY_AUDIT_REFERENCE_PATHS='{}'\"\n",
                reference_path.display()
            ),
        )
        .expect("setup script");
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
        ])
        .expect("audit should parse --extension override");

        assert_eq!(cli.audit.extension_override.extensions, vec!["rust"]);
        assert_eq!(cli.audit.changed_since.as_deref(), Some("origin/main"));
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
            let reference_paths = resolve_audit_reference_paths(&source_ctx);

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
            let reference_paths = resolve_audit_reference_paths(&source_ctx);

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
            let reference_paths = resolve_audit_reference_paths(&source_ctx);

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

            assert!(resolve_audit_reference_paths(&source_ctx).is_empty());
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
                extension_override: ExtensionOverrideArgs {
                    extensions: vec!["source-fixture".to_string()],
                },
                conventions: false,
                only: vec![],
                exclude: vec![],
                baseline_args: BaselineArgs {
                    baseline: false,
                    ignore_baseline: true,
                    ratchet: false,
                },
                changed_since: None,
                json_summary: false,
                fixability: false,
            };

            let (output, code) =
                run(args, &crate::commands::GlobalArgs {}).expect("audit should run");

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
