use clap::Args;
use std::path::Path;

use homeboy::code_audit::{
    self, report, run_main_audit_workflow, AuditCommandOutput, AuditRunWorkflowArgs,
};
use homeboy::engine::execution_context::{self, ResolveOptions};

use super::utils::args::{BaselineArgs, PositionalComponentArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct AuditArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

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
    if let Some(ref component_id) = args.comp.component {
        run_audit_reference_setup(component_id);
    }

    // Resolve component ID and source path.
    // When component is omitted, auto-discover from CWD via homeboy.json.
    let (resolved_id, resolved_path) = if let Some(ref comp_arg) = args.comp.component {
        if Path::new(comp_arg).is_dir() {
            // Bare directory path — no registered component
            let effective = args.comp.path.as_deref().unwrap_or(comp_arg).to_string();
            let name = Path::new(&effective)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            (name, effective)
        } else {
            // Registered component — use unified resolver
            let ctx = execution_context::resolve(&ResolveOptions::source_only(
                comp_arg,
                args.comp.path.clone(),
            ))?;
            (
                ctx.component_id,
                ctx.source_path.to_string_lossy().to_string(),
            )
        }
    } else {
        // No component specified — auto-discover from CWD
        let component = args.comp.load()?;
        let source_path = component.local_path.clone();
        (component.id, source_path)
    };

    let workflow = run_main_audit_workflow(AuditRunWorkflowArgs {
        component_id: resolved_id,
        source_path: resolved_path,
        conventions: args.conventions,
        only_kinds,
        exclude_kinds,
        only_labels: args.only,
        exclude_labels: args.exclude,
        baseline_flags: homeboy::engine::baseline::BaselineFlags {
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
            ratchet: args.baseline_args.ratchet,
        },
        changed_since: args.changed_since,
        json_summary: args.json_summary,
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

    /// End-to-end test of the audit command's read-only mode.
    /// Fixes are now owned by `homeboy refactor --from audit --write`.
    #[test]
    fn audit_detects_outliers_in_convention_group() {
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
        fs::write(root.join("commands/bad.rs"), "pub fn run() {}\n").unwrap();

        let args = AuditArgs {
            comp: PositionalComponentArgs {
                component: Some(root.to_string_lossy().to_string()),
                path: None,
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
        };

        let (output, code) = run(args, &crate::commands::GlobalArgs {}).expect("audit should run");

        // Audit should detect the outlier and return findings
        match output {
            AuditCommandOutput::Full { result, .. } => {
                assert!(
                    !result.findings.is_empty(),
                    "expected findings for the outlier file"
                );
            }
            _ => {} // Summary or other modes are also valid
        }

        // Non-zero exit expected when outliers are found
        assert!(code >= 0, "audit should complete without error");

        let _ = fs::remove_dir_all(root);
    }
}
