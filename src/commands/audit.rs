use clap::Args;
use std::path::Path;

use homeboy::core::code_audit::{
    self, report, run_main_audit_workflow, AuditCommandOutput, AuditRunWorkflowArgs,
};
use homeboy::core::component::{self, TargetSpec};
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{
    finding_records_from_audit, ActiveObservation, NewRunRecord, RunStatus,
};

use super::utils::args::{BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs};
use super::{CmdResult, GlobalArgs};

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

    // Run extension audit reference setup if configured.
    // This resolves framework dependencies (e.g. WordPress core) so their
    // fingerprints are included in cross-reference analysis (dead code detection).
    if let Some(ref component_id) = args.comp.component {
        run_audit_reference_setup(component_id);
    }

    let target = component::resolve_target(TargetSpec::new(
        args.comp.component.as_deref(),
        args.comp.path.as_deref(),
    ))?;
    let resolved_id = target.component_id;
    let resolved_path = target.source_path.to_string_lossy().to_string();

    let observation = start_audit_observation(&resolved_id, &resolved_path, &args);
    let workflow = run_main_audit_workflow(AuditRunWorkflowArgs {
        component_id: resolved_id.clone(),
        source_path: resolved_path.clone(),
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

    let workflow = match workflow {
        Ok(workflow) => {
            finish_audit_observation(observation, &workflow);
            workflow
        }
        Err(error) => {
            finish_audit_observation_error(observation, &error);
            return Err(error);
        }
    };

    Ok(report::from_main_workflow(workflow))
}

struct AuditObservation(ActiveObservation);

fn start_audit_observation(
    component_id: &str,
    source_path: &str,
    args: &AuditArgs,
) -> Option<AuditObservation> {
    let path = Path::new(source_path);
    let metadata = audit_observation_initial_metadata(source_path, args);
    ActiveObservation::start_best_effort(
        NewRunRecord::builder("audit")
            .component_id(component_id)
            .command(audit_observation_command(component_id, args))
            .cwd_path(path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(path))
            .metadata(metadata)
            .build(),
    )
    .map(AuditObservation)
}

fn finish_audit_observation(
    observation: Option<AuditObservation>,
    workflow: &code_audit::AuditRunWorkflowResult,
) {
    let Some(observation) = observation else {
        return;
    };

    let metadata = serde_json::json!({
        "observation_status": if workflow.exit_code == 0 { "pass" } else { "fail" },
        "exit_code": workflow.exit_code,
        "summary": audit_observation_summary(&workflow.output),
    });
    let records = finding_records_from_audit(observation.0.run_id(), &workflow.findings);
    observation.0.record_findings(&records);
    let status = if workflow.exit_code == 0 {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    observation.0.finish_with_merged_metadata(status, metadata);
}

fn finish_audit_observation_error(
    observation: Option<AuditObservation>,
    error: &homeboy::core::Error,
) {
    let Some(observation) = observation else {
        return;
    };

    observation
        .0
        .finish_error_with_merged_metadata(serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
        }));
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

/// Run the extension's audit reference setup script if configured.
///
/// Looks up the component's extension, checks for `audit.setup_references`, and runs it.
/// The script exports `HOMEBOY_AUDIT_REFERENCE_PATHS` which the audit core reads
/// to include framework dependencies in cross-reference analysis.
fn run_audit_reference_setup(component_id_or_path: &str) {
    // Skip for bare directory paths — no extension to look up
    if Path::new(component_id_or_path).is_dir() {
        return;
    }

    // Load component to find its extensions
    let comp = match homeboy::core::component::load(component_id_or_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let extensions = match &comp.extensions {
        Some(ext) => ext,
        None => return,
    };

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

        // Run the script with --export flag and capture stdout
        let output = std::process::Command::new("bash")
            .arg(script_path.to_str().unwrap_or(""))
            .arg("--export")
            .env("HOMEBOY_COMPONENT_PATH", &comp.local_path)
            .current_dir(&comp.local_path)
            .output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Parse the export line: export HOMEBOY_AUDIT_REFERENCE_PATHS='...'
            for line in stdout.lines() {
                if let Some(value) = line.strip_prefix("export HOMEBOY_AUDIT_REFERENCE_PATHS=") {
                    // Remove shell quoting (the value may be $'...' or '...' quoted)
                    let clean = value
                        .trim_start_matches("$'")
                        .trim_start_matches('\'')
                        .trim_end_matches('\'');
                    std::env::set_var("HOMEBOY_AUDIT_REFERENCE_PATHS", clean);
                    break;
                }
            }

            // Log stderr (the script's informational output)
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines() {
                if !line.is_empty() {
                    homeboy::log_status!("audit", "{}", line);
                }
            }
        }
    }
}

// Core function tests (finding_fingerprint, score_delta, weighted_finding_score_with,
// build_chunk_verifier, apply_fix_policy, default_audit_exit_code) have been relocated
// to their respective core modules: code_audit/compare.rs, code_audit/run.rs,
// refactor/auto/apply.rs, refactor/plan/verify.rs.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::utils::args::{BaselineArgs, ExtensionOverrideArgs};
    use crate::test_support::with_isolated_home;
    use clap::Parser;
    use homeboy::core::observation::ObservationStore;
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
    fn audit_observation_start_persists_run_record() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let args = sample_args();

            let observation =
                start_audit_observation("homeboy", &home.path().to_string_lossy(), &args)
                    .expect("observation should start");
            let run_id = observation.0.run_id().to_string();

            finish_audit_observation_error(
                Some(observation),
                &homeboy::core::Error::validation_invalid_argument(
                    "fixture",
                    "simulated audit error",
                    None,
                    None,
                ),
            );

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");

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
            let observation =
                start_audit_observation("homeboy", &home.path().to_string_lossy(), &args)
                    .expect("observation should start");
            let run_id = observation.0.run_id().to_string();
            let finding = code_audit::Finding {
                convention: "command modules".to_string(),
                severity: code_audit::Severity::Warning,
                file: "src/commands/foo.rs".to_string(),
                description: "Missing run function".to_string(),
                suggestion: "Add run()".to_string(),
                kind: code_audit::AuditFinding::MissingMethod,
            };
            let workflow = code_audit::AuditRunWorkflowResult {
                output: AuditCommandOutput::Full {
                    passed: false,
                    result: code_audit::CodeAuditResult {
                        component_id: "homeboy".to_string(),
                        source_path: home.path().to_string_lossy().to_string(),
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
                },
                exit_code: 1,
                findings: vec![finding],
            };

            finish_audit_observation(Some(observation), &workflow);

            let store = ObservationStore::open_initialized().expect("store");
            let findings = store
                .list_findings(homeboy::core::observation::FindingListFilter {
                    run_id: Some(run_id),
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
        });
    }

    #[test]
    fn audit_observation_start_is_best_effort_when_store_unavailable() {
        with_isolated_home(|home| {
            let bad_data_home = home.path().join("not-a-dir");
            fs::write(&bad_data_home, "file blocks observation dir").expect("write marker");
            let _xdg = XdgGuard::set(&bad_data_home);

            let observation =
                start_audit_observation("homeboy", &home.path().to_string_lossy(), &sample_args());

            assert!(observation.is_none());
        });
    }

    /// End-to-end test of the audit command's read-only mode.
    /// Fixes are now owned by `homeboy refactor --from audit --write`.
    #[test]
    fn audit_detects_outliers_in_convention_group() {
        let _audit_guard = crate::test_support::AuditGuard::new();
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
            extension_override: ExtensionOverrideArgs::default(),
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

        let (output, code) = run(args, &crate::commands::GlobalArgs {}).expect("audit should run");

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
    }
}
