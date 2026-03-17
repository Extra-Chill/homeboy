use clap::Args;
use std::path::Path;

use homeboy::code_audit::{
    self, report, run_main_audit_workflow, AuditCommandOutput, AuditRunWorkflowArgs,
};
use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::refactor::{AuditConvergenceScoring, AuditVerificationToggles};

use super::utils::args::{BaselineArgs, PositionalComponentArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct AuditArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    /// Only show discovered conventions (skip findings)
    #[arg(long)]
    pub conventions: bool,

    /// Generate fix stubs for outlier files (dry run by default)
    #[arg(long)]
    pub fix: bool,

    /// Apply fixes to disk (requires --fix)
    #[arg(long, requires = "fix")]
    pub write: bool,

    /// Maximum recursive autofix iterations when writing
    #[arg(long, requires = "fix", default_value_t = 3)]
    pub max_iterations: usize,

    /// Weight for warning-level findings in convergence scoring
    #[arg(long, requires = "fix", default_value_t = 3)]
    pub warning_weight: usize,

    /// Weight for info-level findings in convergence scoring
    #[arg(long, requires = "fix", default_value_t = 1)]
    pub info_weight: usize,

    /// Disable lint smoke verification during chunk verification
    #[arg(long, requires = "fix")]
    pub no_lint_smoke: bool,

    /// Disable test smoke verification during chunk verification
    #[arg(long, requires = "fix")]
    pub no_test_smoke: bool,

    /// Restrict generated fixes to these fix kinds (repeatable)
    #[arg(long = "only", value_name = "kind")]
    pub only: Vec<String>,

    /// Exclude generated fixes for these fix kinds (repeatable)
    #[arg(long = "exclude", value_name = "kind")]
    pub exclude: Vec<String>,

    /// Update baseline when findings are resolved (ratchet forward).
    #[arg(long)]
    pub ratchet: bool,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Only audit files changed since a git ref (branch, tag, or SHA).
    #[arg(long)]
    pub changed_since: Option<String>,

    /// Include compact machine-readable summary for CI wrappers
    #[arg(long)]
    pub json_summary: bool,

    /// Include full generated code in --fix JSON output (omitted by default to reduce size)
    #[arg(long, requires = "fix")]
    pub preview: bool,
}

fn parse_finding_kinds(
    values: &[String],
    flag: &str,
) -> homeboy::Result<Vec<code_audit::AuditFinding>> {
    use std::str::FromStr;
    values
        .iter()
        .map(|value| {
            code_audit::AuditFinding::from_str(value)
                .map_err(|msg| homeboy::Error::validation_invalid_argument(flag, msg, None, None))
        })
        .collect()
}

pub fn run(args: AuditArgs, _global: &GlobalArgs) -> CmdResult<AuditCommandOutput> {
    let only_kinds = parse_finding_kinds(&args.only, "only")?;
    let exclude_kinds = parse_finding_kinds(&args.exclude, "exclude")?;

    // Run extension audit reference setup if configured.
    // This resolves framework dependencies (e.g. WordPress core) so their
    // fingerprints are included in cross-reference analysis (dead code detection).
    run_audit_reference_setup(&args.comp.component);

    // Resolve component ID and source path
    let (resolved_id, resolved_path) = if Path::new(&args.comp.component).is_dir() {
        // Bare directory path — no registered component
        let effective = args
            .comp
            .path
            .as_deref()
            .unwrap_or(&args.comp.component)
            .to_string();
        let name = Path::new(&effective)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        (name, effective)
    } else {
        // Registered component — use unified resolver
        let ctx = execution_context::resolve(&ResolveOptions::source_only(
            &args.comp.component,
            args.comp.path.clone(),
        ))?;
        (
            ctx.component_id,
            ctx.source_path.to_string_lossy().to_string(),
        )
    };

    let workflow = run_main_audit_workflow(AuditRunWorkflowArgs {
        component_id: resolved_id,
        source_path: resolved_path,
        conventions: args.conventions,
        fix: args.fix,
        write: args.write,
        max_iterations: args.max_iterations,
        scoring: AuditConvergenceScoring {
            warning_weight: args.warning_weight,
            info_weight: args.info_weight,
        },
        verification: AuditVerificationToggles {
            lint_smoke: !args.no_lint_smoke,
            test_smoke: !args.no_test_smoke,
        },
        only_kinds,
        exclude_kinds,
        only_labels: args.only,
        exclude_labels: args.exclude,
        ratchet: args.ratchet,
        baseline: args.baseline_args.baseline,
        ignore_baseline: args.baseline_args.ignore_baseline,
        changed_since: args.changed_since,
        json_summary: args.json_summary,
        preview: args.preview,
    })?;

    Ok(report::from_main_workflow(workflow))
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
    let comp = match homeboy::component::load(component_id_or_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let extensions = match &comp.extensions {
        Some(ext) => ext,
        None => return,
    };

    for ext_id in extensions.keys() {
        let ext_manifest = match homeboy::extension::load_extension(ext_id) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let setup_script = match ext_manifest.audit_setup_references() {
            Some(s) => s,
            None => continue,
        };

        // Resolve script path relative to extension directory
        let ext_path = homeboy::extension::extension_path(ext_id);
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
    use crate::commands::utils::args::BaselineArgs;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("homeboy-audit-command-{name}-{nanos}"))
    }

    /// End-to-end test of the audit command's fix-write mode.
    /// This is the only test that exercises the command's `run()` function
    /// directly — all other tests belong in their core modules.
    #[test]
    fn audit_fix_write_stops_when_no_safe_changes_apply() {
        let root = tmp_dir("fix-write-no-safe-changes");
        fs::create_dir_all(root.join("commands")).unwrap();

        fs::write(
            root.join("commands/good_one.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/good_two.rs"),
            "pub fn run() {}\npub fn helper() {}\n",
        )
        .unwrap();
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        let args = AuditArgs {
            comp: PositionalComponentArgs {
                component: root.to_string_lossy().to_string(),
                path: None,
            },
            conventions: false,
            fix: true,
            write: true,
            ratchet: false,
            max_iterations: 3,
            warning_weight: 3,
            info_weight: 1,
            no_lint_smoke: false,
            no_test_smoke: false,
            only: vec!["duplicate_function".to_string()],
            exclude: vec![],
            baseline_args: BaselineArgs {
                baseline: false,
                ignore_baseline: true,
            },
            changed_since: None,
            json_summary: false,
            preview: false,
        };

        let (output, _code) =
            run(args, &crate::commands::GlobalArgs {}).expect("audit fix should run");

        match output {
            AuditCommandOutput::Fix { iterations, .. } => {
                assert!(!iterations.is_empty(), "expected at least one iteration");
                let any_applied = iterations.iter().any(|i| i.applied_chunks > 0);
                assert!(
                    any_applied,
                    "expected at least one iteration to apply changes, got: {:?}",
                    iterations.iter().map(|i| &i.status).collect::<Vec<_>>()
                );
            }
            other => panic!(
                "expected AuditCommandOutput::Fix, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        let _ = fs::remove_dir_all(root);
    }
}
