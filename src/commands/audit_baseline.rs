use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

use homeboy::core::code_audit::{
    baseline, run_main_audit_workflow, AuditCommandOutput, AuditRunWorkflowArgs,
};

use super::source_command::resolve_source_context;
use super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct AuditBaselineArgs {
    #[command(subcommand)]
    command: AuditBaselineCommand,
}

#[derive(Subcommand)]
enum AuditBaselineCommand {
    /// Refresh only persisted audit baseline data for changed files
    Refresh(AuditBaselineRefreshArgs),
}

#[derive(Args)]
pub struct AuditBaselineRefreshArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    /// Refresh baseline entries for files changed since this git ref
    #[arg(long, default_value = "origin/main")]
    pub changed_since: String,
}

#[derive(Debug, Serialize)]
pub struct AuditBaselineRefreshOutput {
    pub command: String,
    pub component_id: String,
    pub source_path: String,
    pub baseline_path: String,
    pub changed_since: String,
    pub previous_source: String,
    pub previous_count: usize,
    pub current_count: usize,
    pub added_count: usize,
    pub resolved_count: usize,
    pub added_fingerprints: Vec<String>,
    pub resolved_fingerprints: Vec<String>,
}

pub fn run(args: AuditBaselineArgs, global: &GlobalArgs) -> CmdResult<AuditBaselineRefreshOutput> {
    match args.command {
        AuditBaselineCommand::Refresh(args) => refresh(args, global),
    }
}

fn refresh(
    args: AuditBaselineRefreshArgs,
    _global: &GlobalArgs,
) -> CmdResult<AuditBaselineRefreshOutput> {
    let source_ctx = resolve_source_context(
        &args.comp,
        &SettingArgs::default(),
        &args.extension_override,
        None,
    )?;
    let reference_paths = super::audit::resolve_audit_reference_paths(&source_ctx);
    let source_path = source_ctx.source_path.to_string_lossy().to_string();
    let source = Path::new(&source_path);

    fail_if_homeboy_json_has_conflict_markers(source)?;

    let (previous_source, previous_fingerprints) =
        previous_baseline_fingerprints(source, &source_path, &args.changed_since);

    let workflow = run_main_audit_workflow(AuditRunWorkflowArgs {
        component_id: source_ctx.component_id.clone(),
        source_path: source_path.clone(),
        reference_paths,
        conventions: false,
        only_kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        only_labels: Vec::new(),
        exclude_labels: Vec::new(),
        extension_overrides: args.extension_override.extensions,
        baseline_flags: homeboy::core::engine::baseline::BaselineFlags {
            baseline: true,
            ignore_baseline: false,
            ratchet: false,
        },
        changed_since: Some(args.changed_since.clone()),
        json_summary: false,
        include_fixability: false,
    })?;

    let current = baseline::load_baseline(source).ok_or_else(|| {
        homeboy::core::Error::internal_unexpected("Audit baseline refresh did not write a baseline")
    })?;
    let baseline_path = match workflow.output {
        AuditCommandOutput::BaselineSaved { path, .. } => path,
        _ => source.join("homeboy.json").to_string_lossy().to_string(),
    };

    let current_fingerprints = current
        .known_fingerprints
        .into_iter()
        .collect::<BTreeSet<_>>();
    let added_fingerprints = sorted_difference(&current_fingerprints, &previous_fingerprints);
    let resolved_fingerprints = sorted_difference(&previous_fingerprints, &current_fingerprints);

    eprintln!(
        "[audit-baseline] refreshed {}: +{} / -{} fingerprints",
        baseline_path,
        added_fingerprints.len(),
        resolved_fingerprints.len()
    );

    let output = AuditBaselineRefreshOutput {
        command: "audit-baseline.refresh".to_string(),
        component_id: source_ctx.component_id,
        source_path,
        baseline_path,
        changed_since: args.changed_since,
        previous_source,
        previous_count: previous_fingerprints.len(),
        current_count: current_fingerprints.len(),
        added_count: added_fingerprints.len(),
        resolved_count: resolved_fingerprints.len(),
        added_fingerprints,
        resolved_fingerprints,
    };

    Ok((output, workflow.exit_code))
}

fn previous_baseline_fingerprints(
    source: &Path,
    source_path: &str,
    changed_since: &str,
) -> (String, BTreeSet<String>) {
    if let Some(local) = baseline::load_baseline(source) {
        return (
            "working-tree homeboy.json".to_string(),
            local.known_fingerprints.into_iter().collect(),
        );
    }

    if let Some(from_ref) = baseline::load_baseline_from_ref(source_path, changed_since) {
        return (
            format!("{changed_since}:homeboy.json"),
            from_ref.known_fingerprints.into_iter().collect(),
        );
    }

    ("none".to_string(), BTreeSet::new())
}

fn sorted_difference(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Vec<String> {
    left.difference(right).cloned().collect()
}

fn fail_if_homeboy_json_has_conflict_markers(source: &Path) -> homeboy::core::Result<()> {
    let path = source.join("homeboy.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(());
    };

    if content.contains("<<<<<<<") || content.contains("=======") || content.contains(">>>>>>>") {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "homeboy.json",
            format!(
                "{} contains merge conflict markers. Resolve non-baseline config first, then run `homeboy audit-baseline refresh --path {} --changed-since origin/main`.",
                path.display(),
                source.display()
            ),
            None,
            None,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn sorted_difference_reports_added_fingerprints_deterministically() {
        assert_eq!(
            sorted_difference(&set(&["b", "a", "c"]), &set(&["b"])),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn conflict_marker_preflight_rejects_unparseable_homeboy_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("homeboy.json"),
            "{\n<<<<<<< HEAD\n}\n=======\n{}\n>>>>>>> main\n",
        )
        .expect("write fixture");

        let error = fail_if_homeboy_json_has_conflict_markers(dir.path())
            .expect_err("conflict markers should fail preflight");

        assert!(error.to_string().contains("merge conflict markers"));
    }
}
