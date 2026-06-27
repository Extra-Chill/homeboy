use std::path::PathBuf;

use clap::{Args, Subcommand};
use homeboy::core::cleanup::{self, ArtifactCleanupOptions, ArtifactCleanupOutput};
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

    /// Only reclaim artifacts from worktrees whose branch is already merged
    /// into its upstream. Preserves in-progress cooks' build dirs.
    #[arg(long)]
    pub merged_only: bool,
}

pub fn run(args: CleanupArgs, _global: &super::GlobalArgs) -> CmdResult<ArtifactCleanupOutput> {
    match args.command {
        CleanupCommand::Artifacts(args) => cleanup::cleanup_artifacts(ArtifactCleanupOptions {
            path: args.path,
            apply: args.apply,
            self_artifacts: args.self_artifacts,
            temp_roots: args.temp_root,
            merged_only: args.merged_only,
        })
        .map(|output| (output, 0)),
    }
}

pub(crate) fn render_artifact_cleanup_summary(payload: &Value) -> Option<String> {
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
    use serde_json::json;

    use super::*;

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
}
