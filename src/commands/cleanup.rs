use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use homeboy::core::cleanup::{
    self, ArtifactCleanupOptions, ArtifactCleanupSort, ResourceCleanupOptions,
};
use homeboy::core::defaults;
use homeboy::core::resource_cleanup_intent::ResourceCleanupIntent;
use homeboy::core::worktree_providers::WorktreeProviderCleanupOptions;
use serde_json::Value;

use super::CmdResult;

#[derive(Args)]
pub struct CleanupArgs {
    #[command(subcommand)]
    pub command: CleanupCommand,
}

#[derive(Subcommand)]
pub enum CleanupCommand {
    /// Inspect or remove declared reconstructable artifacts across repo worktrees
    Artifacts(CleanupArtifactsArgs),

    /// Aggregate cleanup across configured external worktree providers
    Worktrees(CleanupWorktreesArgs),
}

#[derive(Args)]
pub struct CleanupArtifactsArgs {
    /// Apply cleanup. Omit for dry-run output.
    #[arg(long)]
    pub apply: bool,

    /// Clean artifacts from the Homeboy source checkout that built this binary.
    #[arg(long = "self", conflicts_with = "path")]
    pub self_artifacts: bool,

    /// Resolve managed worktrees from this checkout instead of the current directory.
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Also scan this temp root for detached Homeboy build artifacts. Repeatable.
    #[arg(long, value_name = "PATH")]
    pub temp_root: Vec<PathBuf>,

    /// Sort artifact candidates before reporting or applying cleanup.
    #[arg(long, value_enum, default_value = "discovery")]
    pub sort: CleanupArtifactsSortArg,

    /// Limit artifact candidates reported or removed after sorting.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream. Preserves in-progress cooks' build dirs.
    #[arg(long)]
    pub merged_only: bool,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum CleanupArtifactsSortArg {
    #[default]
    Discovery,
    Size,
}

#[derive(Args)]
pub struct CleanupWorktreesArgs {
    /// Cleanup a specific configured provider. Repeatable.
    #[arg(long = "provider", value_name = "ID", conflicts_with = "all_providers")]
    pub provider: Vec<String>,

    /// Cleanup every enabled configured provider.
    #[arg(long)]
    pub all_providers: bool,

    /// Apply cleanup. Omit for provider preview/dry-run output.
    #[arg(long)]
    pub apply: bool,
}

pub fn run(args: CleanupArgs, _global: &super::GlobalArgs) -> CmdResult<Value> {
    match args.command {
        CleanupCommand::Artifacts(args) => cleanup::cleanup_resources_from_config(
            ResourceCleanupOptions {
                intent: cleanup_intent(args.apply),
                artifacts: Some(ArtifactCleanupOptions {
                    path: args.path,
                    apply: args.apply,
                    self_artifacts: args.self_artifacts,
                    temp_roots: args.temp_root,
                    sort: match args.sort {
                        CleanupArtifactsSortArg::Discovery => ArtifactCleanupSort::Discovery,
                        CleanupArtifactsSortArg::Size => ArtifactCleanupSort::Size,
                    },
                    limit: args.limit,
                    merged_only: args.merged_only,
                }),
                worktree_providers: None,
            },
            defaults::load_config(),
        )
        .and_then(|output| {
            serde_json::to_value(output).map_err(|err| {
                homeboy::core::Error::internal_json(
                    err.to_string(),
                    Some("serialize cleanup artifacts output".to_string()),
                )
            })
        })
        .map(|output| (output, 0)),
        CleanupCommand::Worktrees(args) => cleanup::cleanup_resources_from_config(
            ResourceCleanupOptions {
                intent: cleanup_intent(args.apply),
                artifacts: None,
                worktree_providers: Some(WorktreeProviderCleanupOptions {
                    provider: args.provider,
                    all_providers: args.all_providers,
                    apply: args.apply,
                }),
            },
            defaults::load_config(),
        )
        .and_then(|output| {
            serde_json::to_value(output).map_err(|err| {
                homeboy::core::Error::internal_json(
                    err.to_string(),
                    Some("serialize cleanup worktrees output".to_string()),
                )
            })
        })
        .map(|output| (output, 0)),
    }
}

fn cleanup_intent(apply: bool) -> ResourceCleanupIntent {
    if apply {
        ResourceCleanupIntent::Apply
    } else {
        ResourceCleanupIntent::DryRun
    }
}

pub(crate) fn render_artifact_cleanup_summary(payload: &Value) -> Option<String> {
    let payload = if payload.get("command").and_then(Value::as_str)? == "cleanup.resources" {
        payload.get("artifacts")?
    } else {
        payload
    };

    if payload.get("command").and_then(Value::as_str)? != "cleanup.artifacts" {
        return None;
    }

    let mode = payload.get("mode").and_then(Value::as_str)?;
    let root = payload.get("root").and_then(Value::as_str).unwrap_or(".");
    let candidate_count = payload
        .get("candidate_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let applied_count = payload
        .get("applied_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let skipped_count = payload
        .get("skipped_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let estimated_bytes = payload
        .get("estimated_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reclaimed_bytes = payload
        .get("reclaimed_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let remaining_candidates = candidate_count.saturating_sub(applied_count);

    let mut lines = vec![
        "Artifact cleanup summary".to_string(),
        format!(
            "Mode: {}",
            if mode == "apply" { "apply" } else { "dry run" }
        ),
        format!("Root: {root}"),
        format!("Candidates: {candidate_count}"),
        format!("Applied: {applied_count}"),
        format!("Remaining candidates: {remaining_candidates}"),
        format!("Estimated reclaimable: {}", format_bytes(estimated_bytes)),
        format!("Reclaimed: {}", format_bytes(reclaimed_bytes)),
        format!("Skipped: {skipped_count}"),
    ];

    for (reason, count) in skipped_counts_by_reason(payload) {
        lines.push(format!("  - {reason}: {count}"));
    }

    let candidate_display_limit = 10;
    let candidate_lines = artifact_candidate_lines(payload, candidate_display_limit);
    if !candidate_lines.is_empty() {
        lines.push(format!(
            "Rebuildable artifacts (showing {} of {candidate_count}):",
            candidate_lines.len()
        ));
        lines.extend(candidate_lines);
        if candidate_count > candidate_display_limit as u64 {
            lines.push(format!(
                "Full candidate list is available in JSON output; use --sort size --limit {candidate_display_limit} for a bounded largest-first review."
            ));
        }
    }

    let next = if mode == "apply" {
        format!("homeboy cleanup artifacts --path {}", shell_quote(root))
    } else {
        format!(
            "homeboy cleanup artifacts --path {} --apply",
            shell_quote(root)
        )
    };
    lines.push(format!("Next safe command: {next}"));
    lines.push(String::new());

    Some(lines.join("\n"))
}

pub(crate) fn render_cleanup_summary(payload: &Value) -> Option<String> {
    render_artifact_cleanup_summary(payload).or_else(|| render_worktree_cleanup_summary(payload))
}

pub(crate) fn render_worktree_cleanup_summary(payload: &Value) -> Option<String> {
    let payload = if payload.get("command").and_then(Value::as_str)? == "cleanup.resources" {
        payload.get("worktree_providers")?
    } else {
        payload
    };

    if payload.get("command").and_then(Value::as_str)? != "cleanup.worktrees" {
        return None;
    }

    let mode = payload
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("preview");
    let provider_count = payload
        .get("provider_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let success_count = payload
        .get("success_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let failure_count = payload
        .get("failure_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let mut lines = vec![
        "Worktree provider cleanup summary".to_string(),
        format!(
            "Mode: {}",
            if mode == "apply" { "apply" } else { "preview" }
        ),
        format!("Providers: {provider_count}"),
        format!("Succeeded: {success_count}"),
        format!("Failed: {failure_count}"),
    ];

    if let Some(providers) = payload.get("providers").and_then(Value::as_array) {
        for provider in providers {
            let provider_id = provider
                .get("provider_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let success = provider
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            lines.push(format!(
                "Provider {provider_id}: {}",
                if success { "ok" } else { "failed" }
            ));
            if let Some(command) = provider_command(provider) {
                lines.push(format!("  Command: {command}"));
            }
            if let Some(phase) = provider.get("phase").and_then(Value::as_str) {
                lines.push(format!("  Phase: {phase}"));
            }
            if let Some(progress) = provider.get("last_progress").and_then(Value::as_str) {
                lines.push(format!("  Last observed progress: {progress}"));
            }
            if let Some(run_refs) = provider.get("run_refs").and_then(Value::as_array) {
                for run_ref in run_refs {
                    if let Some(run_id) = run_ref.get("run_id").and_then(Value::as_str) {
                        lines.push(format!("  Run: {run_id}"));
                    }
                    if let Some(status_command) =
                        run_ref.get("status_command").and_then(Value::as_str)
                    {
                        lines.push(format!("  Status command: {status_command}"));
                    }
                }
            }
            if let Some(follow_up) = provider.get("follow_up_command").and_then(Value::as_str) {
                lines.push(format!("  Safe follow-up command: {follow_up}"));
            }
            if let Some(error) = provider.get("error").and_then(Value::as_str) {
                lines.push(format!("  Error: {error}"));
            }
        }
    }

    lines.push(String::new());
    Some(lines.join("\n"))
}

fn provider_command(provider: &Value) -> Option<String> {
    let argv = provider.get("command_run")?.as_array()?;
    let parts: Vec<String> = argv
        .iter()
        .filter_map(Value::as_str)
        .map(shell_quote)
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn artifact_candidate_lines(payload: &Value, limit: usize) -> Vec<String> {
    payload
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
        .filter_map(|row| {
            let path = row.get("path").and_then(Value::as_str)?;
            let bytes = row.get("size_bytes").and_then(Value::as_u64).unwrap_or(0);
            Some(format!("  - {} {}", format_bytes(bytes), path))
        })
        .collect()
}

fn skipped_counts_by_reason(payload: &Value) -> Vec<(String, u64)> {
    let mut counts = std::collections::BTreeMap::new();
    if let Some(skipped) = payload.get("skipped").and_then(Value::as_array) {
        for row in skipped {
            if let Some(reason) = row.get("reason").and_then(Value::as_str) {
                *counts.entry(reason.to_string()).or_insert(0) += 1;
            }
        }
    }
    counts.into_iter().collect()
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    match bytes {
        0..=1023 => format!("{bytes} B"),
        _ if (bytes as f64) < MIB => format!("{:.1} KiB", bytes as f64 / KIB),
        _ if (bytes as f64) < GIB => format!("{:.1} MiB", bytes as f64 / MIB),
        _ => format!("{:.1} GiB", bytes as f64 / GIB),
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::json;

    use crate::cli_surface::{Cli, Commands};

    use super::*;

    #[test]
    fn cleanup_artifacts_cli_accepts_bounded_review_flags() {
        let cli = Cli::parse_from([
            "homeboy",
            "cleanup",
            "artifacts",
            "--sort",
            "size",
            "--limit",
            "7",
            "--merged-only",
        ]);

        let Commands::Cleanup(args) = cli.command else {
            panic!("expected cleanup command");
        };
        let CleanupCommand::Artifacts(args) = args.command else {
            panic!("expected cleanup artifacts command");
        };
        assert!(matches!(args.sort, CleanupArtifactsSortArg::Size));
        assert_eq!(args.limit, Some(7));
        assert!(args.merged_only);
    }

    #[test]
    fn cleanup_artifacts_summary_emphasizes_operator_counts() {
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "dry_run",
            "root": "/tmp/homeboy repo",
            "worktree_count": 2,
            "candidate_count": 3,
            "skipped_count": 2,
            "applied_count": 0,
            "estimated_bytes": 1572864,
            "reclaimed_bytes": 0,
            "candidates": [],
            "skipped": [
                { "reason": "artifact path contains tracked or staged source changes" },
                { "reason": "artifact path contains tracked or staged source changes" }
            ],
            "applied": []
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Artifact cleanup summary\n"));
        assert!(summary.contains("Candidates: 3\n"));
        assert!(summary.contains("Applied: 0\n"));
        assert!(summary.contains("Remaining candidates: 3\n"));
        assert!(summary.contains("Estimated reclaimable: 1.5 MiB\n"));
        assert!(summary.contains("Reclaimed: 0 B\n"));
        assert!(
            summary.contains("  - artifact path contains tracked or staged source changes: 2\n")
        );
        assert!(summary.contains(
            "Next safe command: homeboy cleanup artifacts --path '/tmp/homeboy repo' --apply\n"
        ));
    }

    #[test]
    fn cleanup_artifacts_apply_summary_reports_remaining_after_applied() {
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "apply",
            "root": "/tmp/homeboy",
            "candidate_count": 4,
            "skipped_count": 1,
            "applied_count": 3,
            "estimated_bytes": 4096,
            "reclaimed_bytes": 3072,
            "skipped": [
                { "reason": "worktree branch is not merged into its upstream" }
            ]
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Mode: apply\n"));
        assert!(summary.contains("Remaining candidates: 1\n"));
        assert!(summary.contains("Reclaimed: 3.0 KiB\n"));
        assert!(
            summary.contains("Next safe command: homeboy cleanup artifacts --path /tmp/homeboy\n")
        );
    }

    #[test]
    fn cleanup_artifacts_summary_lists_candidates_in_payload_order() {
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "dry_run",
            "root": "/tmp/repo",
            "candidate_count": 2,
            "skipped_count": 0,
            "applied_count": 0,
            "estimated_bytes": 3072,
            "reclaimed_bytes": 0,
            "candidates": [
                { "path": "/tmp/repo/node_modules", "size_bytes": 2048 },
                { "path": "/tmp/repo/dist", "size_bytes": 1024 }
            ],
            "skipped": []
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Rebuildable artifacts (showing 2 of 2):"));
        let first = summary.find("  - 2.0 KiB /tmp/repo/node_modules").unwrap();
        let second = summary.find("  - 1.0 KiB /tmp/repo/dist").unwrap();
        assert!(first < second);
    }

    #[test]
    fn cleanup_artifacts_summary_marks_truncated_candidate_list() {
        let candidates: Vec<_> = (0..12)
            .map(|index| {
                json!({
                    "path": format!("/tmp/repo/target-{index}"),
                    "size_bytes": 1024
                })
            })
            .collect();
        let payload = json!({
            "command": "cleanup.artifacts",
            "mode": "dry_run",
            "root": "/tmp/repo",
            "candidate_count": 12,
            "skipped_count": 0,
            "applied_count": 0,
            "estimated_bytes": 12288,
            "reclaimed_bytes": 0,
            "candidates": candidates,
            "skipped": []
        });

        let summary = render_artifact_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Rebuildable artifacts (showing 10 of 12):"));
        assert!(summary.contains("Full candidate list is available in JSON output"));
        assert!(summary.contains("--sort size --limit 10"));
        assert!(!summary.contains("/tmp/repo/target-10"));
    }

    #[test]
    fn cleanup_worktrees_summary_surfaces_provider_progress_and_refs() {
        let payload = json!({
            "command": "cleanup.resources",
            "mode": "apply",
            "worktree_providers": {
                "command": "cleanup.worktrees",
                "mode": "apply",
                "provider_count": 1,
                "success_count": 1,
                "failure_count": 0,
                "providers": [
                    {
                        "provider_id": "fixture",
                        "success": true,
                        "mode": "apply",
                        "command_run": ["provider-bin", "cleanup", "--apply"],
                        "phase": "running",
                        "last_progress": "removed 10/20",
                        "run_refs": [
                            {
                                "run_id": "cleanup-run-1",
                                "status_command": "provider status cleanup-run-1"
                            }
                        ],
                        "follow_up_command": "provider status cleanup-run-1"
                    }
                ]
            }
        });

        let summary = render_worktree_cleanup_summary(&payload).expect("summary");

        assert!(summary.contains("Worktree provider cleanup summary\n"));
        assert!(summary.contains("Mode: apply\n"));
        assert!(summary.contains("Provider fixture: ok\n"));
        assert!(summary.contains("  Command: provider-bin cleanup --apply\n"));
        assert!(summary.contains("  Phase: running\n"));
        assert!(summary.contains("  Last observed progress: removed 10/20\n"));
        assert!(summary.contains("  Run: cleanup-run-1\n"));
        assert!(summary.contains("  Status command: provider status cleanup-run-1\n"));
        assert!(summary.contains("  Safe follow-up command: provider status cleanup-run-1\n"));
    }
}
