use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

use homeboy::core::code_audit::{
    baseline, merge_baseline_only_conflict, run_main_audit_workflow, AuditCommandOutput,
    AuditRunWorkflowArgs, BaselineMergeError,
};
use homeboy::core::engine::command::run_in_optional;

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
    /// Auto-merge a baseline-only `homeboy.json` merge/rebase conflict
    Merge(AuditBaselineMergeArgs),
}

#[derive(Args)]
pub struct AuditBaselineMergeArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,
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
        AuditBaselineCommand::Merge(args) => merge(args, global),
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
        profile: homeboy::core::code_audit::AuditProfile::Full,
        extension_overrides: args.extension_override.extensions,
        baseline_flags: homeboy::core::engine::baseline::BaselineFlags {
            baseline: true,
            ignore_baseline: false,
            ratchet: false,
        },
        changed_since: Some(args.changed_since.clone()),
        precomputed_changed_files: None,
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

fn merge(
    args: AuditBaselineMergeArgs,
    _global: &GlobalArgs,
) -> CmdResult<AuditBaselineRefreshOutput> {
    let source_ctx = resolve_source_context(
        &args.comp,
        &SettingArgs::default(),
        &args.extension_override,
        None,
    )?;
    let source_path = source_ctx.source_path.to_string_lossy().to_string();
    let source = Path::new(&source_path);
    let baseline_path = source.join("homeboy.json").to_string_lossy().to_string();

    // Require an actual conflicted homeboy.json (an in-progress merge/rebase with
    // unmerged stages for the path). Without unmerged stages there is nothing to
    // auto-merge — point the user at refresh instead.
    if !homeboy_json_is_conflicted(&source_path) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "homeboy.json",
            format!(
                "{baseline_path} has no in-progress merge conflict to resolve. If main moved, run `homeboy audit-baseline refresh {source_path} --changed-since origin/main` instead."
            ),
            None,
            None,
        ));
    }

    let ours = parse_conflict_stage(&source_path, 2, "ours")?;
    let theirs = parse_conflict_stage(&source_path, 3, "theirs")?;
    let base = parse_optional_conflict_stage(&source_path, 1);

    let result = merge_baseline_only_conflict(base.as_ref(), &ours, &theirs)
        .map_err(map_baseline_merge_error)?;

    let merged_content = serde_json::to_string_pretty(&result.merged).map_err(|error| {
        homeboy::core::Error::internal_io(
            format!("Failed to serialize merged homeboy.json: {error}"),
            Some("audit-baseline.merge".to_string()),
        )
    })?;
    std::fs::write(&baseline_path, format!("{merged_content}\n")).map_err(|error| {
        homeboy::core::Error::internal_io(
            format!("Failed to write {baseline_path}: {error}"),
            Some("audit-baseline.merge".to_string()),
        )
    })?;

    // Mark the path resolved so the in-progress merge/rebase can continue.
    let _ = run_in_optional(&source_path, "git", &["add", "homeboy.json"]);

    eprintln!(
        "[audit-baseline] merged baseline conflict in {}: +{} / -{} fingerprints",
        baseline_path,
        result.added_fingerprints.len(),
        result.resolved_fingerprints.len()
    );

    let base_fingerprints = base
        .as_ref()
        .map(audit_fingerprints_from_doc)
        .unwrap_or_default();
    let merged_fingerprints = audit_fingerprints_from_doc(&result.merged);

    let output = AuditBaselineRefreshOutput {
        command: "audit-baseline.merge".to_string(),
        component_id: source_ctx.component_id,
        source_path,
        baseline_path,
        changed_since: "merge-conflict".to_string(),
        previous_source: "conflict base (:1:)".to_string(),
        previous_count: base_fingerprints.len(),
        current_count: merged_fingerprints.len(),
        added_count: result.added_fingerprints.len(),
        resolved_count: result.resolved_fingerprints.len(),
        added_fingerprints: result.added_fingerprints,
        resolved_fingerprints: result.resolved_fingerprints,
    };

    Ok((output, 0))
}

/// True when `homeboy.json` has unmerged conflict stages recorded in the index.
fn homeboy_json_is_conflicted(source_path: &str) -> bool {
    run_in_optional(
        source_path,
        "git",
        &["ls-files", "-u", "--", "homeboy.json"],
    )
    .map(|output| !output.trim().is_empty())
    .unwrap_or(false)
}

/// Read and parse a required conflict stage (`:<stage>:homeboy.json`).
fn parse_conflict_stage(
    source_path: &str,
    stage: u8,
    label: &'static str,
) -> homeboy::core::Result<serde_json::Value> {
    let spec = format!(":{stage}:homeboy.json");
    let content = run_in_optional(source_path, "git", &["show", &spec]).ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "homeboy.json",
            format!("Could not read {label} side of the conflict ({spec})."),
            None,
            None,
        )
    })?;

    serde_json::from_str(&content).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "homeboy.json",
            format!("{label} side of homeboy.json is not valid JSON: {error}"),
            None,
            None,
        )
    })
}

/// Read and parse an optional conflict stage; absent or unparseable → `None`.
///
/// The merge-base stage (`:1:`) is absent for add/add conflicts, which is fine —
/// the merge falls back to computing counts against `ours`.
fn parse_optional_conflict_stage(source_path: &str, stage: u8) -> Option<serde_json::Value> {
    let spec = format!(":{stage}:homeboy.json");
    let content = run_in_optional(source_path, "git", &["show", &spec])?;
    serde_json::from_str(&content).ok()
}

/// Pull the flat `baselines.audit.known_fingerprints` list from a merged document.
fn audit_fingerprints_from_doc(doc: &serde_json::Value) -> BTreeSet<String> {
    doc.get("baselines")
        .and_then(|baselines| baselines.get("audit"))
        .and_then(|audit| audit.get("known_fingerprints"))
        .and_then(|value| value.as_array())
        .map(|array| {
            array
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn map_baseline_merge_error(error: BaselineMergeError) -> homeboy::core::Error {
    match &error {
        BaselineMergeError::NonBaselineConflict { .. } => {
            homeboy::core::Error::validation_invalid_argument(
                "homeboy.json",
                format!(
                    "{error} Auto-merge only handles generated audit baseline data; resolve the listed keys by hand, then `git add homeboy.json` and continue."
                ),
                None,
                None,
            )
        }
        BaselineMergeError::InvalidJson { .. } => homeboy::core::Error::validation_invalid_argument(
            "homeboy.json",
            error.to_string(),
            None,
            None,
        ),
    }
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
