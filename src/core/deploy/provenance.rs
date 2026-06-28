//! Build provenance tracking.
//!
//! Provides tag-gap detection so `build` and `deploy` can warn about
//! unreleased commits ahead of the latest tag, plus per-deploy build provenance
//! capture (which source/ref produced the payload, whether a fresh build ran, and
//! the identity of the artifact that shipped).

use std::path::Path;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::core::component::Component;
use crate::core::engine::command;
use crate::core::git;

use super::generated_artifacts::uncommitted_file_report_excluding_known_generated;
use super::types::{ArtifactIdentity, BuildProvenance, BuildSource};

/// Capture explicit build provenance for a prepared deploy.
///
/// Called once per component during preflight (while the source tree is at the
/// built state and the artifact still exists), so the deploy result can report,
/// consistently across every strategy, exactly what was built and shipped.
/// `built_from_ref` is left unset here and filled in by the orchestrator, which
/// owns the deployed tag/branch label.
pub(super) fn capture_build_provenance(
    component: &Component,
    source: BuildSource,
    build_ran: bool,
    artifact_path: Option<&Path>,
) -> BuildProvenance {
    let local_path = &component.local_path;
    let built_from_commit = command::run_in_optional(local_path, "git", &["rev-parse", "HEAD"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    // A downloaded release is not built from the local tree, so local dirtiness is
    // not meaningful provenance for it.
    let working_tree_dirty = match source {
        BuildSource::DownloadedRelease => None,
        _ => uncommitted_file_report_excluding_known_generated(component)
            .ok()
            .map(|report| !report.unexpected.is_empty()),
    };

    let artifact_identity = artifact_path.and_then(resolve_artifact_identity);

    BuildProvenance {
        source,
        build_ran,
        built_from_ref: None,
        built_from_commit,
        working_tree_dirty,
        artifact_identity,
    }
}

fn resolve_artifact_identity(path: &Path) -> Option<ArtifactIdentity> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs() as i64);

    if metadata.is_file() {
        Some(ArtifactIdentity {
            path: path.to_string_lossy().to_string(),
            size_bytes: Some(metadata.len()),
            sha256: sha256_file(path),
            modified_unix,
        })
    } else {
        // Directory artifacts (e.g. rsync of a tree) have no single-file hash.
        Some(ArtifactIdentity {
            path: path.to_string_lossy().to_string(),
            size_bytes: None,
            sha256: None,
            modified_unix,
        })
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

/// Information about the HEAD-vs-tag gap for a component.
#[derive(Debug, Clone)]
pub struct TagGap {
    /// The latest tag
    pub tag: String,
    /// Number of commits HEAD is ahead
    pub ahead: u32,
    /// Short commit subjects (newest first)
    pub commits: Vec<String>,
}

/// Check if HEAD is ahead of the latest tag for a component.
/// Returns None if HEAD is at or behind the tag, or if no tags exist.
pub fn detect_tag_gap(component: &Component) -> Option<TagGap> {
    let path = &component.local_path;
    let tag = git::get_latest_tag(path).ok().flatten()?;

    let ahead_str = command::run_in_optional(
        path,
        "git",
        &["rev-list", "--count", &format!("{}..HEAD", tag)],
    )?;
    let ahead = ahead_str.trim().parse::<u32>().ok()?;

    if ahead == 0 {
        return None;
    }

    // Get commit subjects for the unreleased commits (max 10)
    let log_output = command::run_in_optional(
        path,
        "git",
        &[
            "log",
            "--oneline",
            "--format=%h %s",
            "-10",
            &format!("{}..HEAD", tag),
        ],
    )
    .unwrap_or_default();

    let commits: Vec<String> = log_output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Some(TagGap {
        tag,
        ahead,
        commits,
    })
}

/// Format a tag gap as a human-readable warning string.
///
/// Used by both `build` and `deploy` commands.
fn format_tag_gap(component_id: &str, gap: &TagGap, context: &str) -> String {
    let mut lines = vec![format!(
        "[{}] '{}': HEAD is {} commit(s) ahead of latest tag {}",
        context, component_id, gap.ahead, gap.tag
    )];
    for commit in &gap.commits {
        lines.push(format!("[{}]      {}", context, commit));
    }
    if gap.ahead > 10 {
        lines.push(format!(
            "[{}]      ... and {} more",
            context,
            gap.ahead - gap.commits.len() as u32
        ));
    }
    lines.join("\n")
}

/// Print a tag gap warning to stderr. Always prints regardless of TTY.
pub fn warn_tag_gap(component_id: &str, gap: &TagGap, context: &str) {
    eprintln!("{}", format_tag_gap(component_id, gap, context));
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn committed_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q", "-b", "main"]);
        run_git(dir, &["config", "user.email", "homeboy@example.com"]);
        run_git(dir, &["config", "user.name", "Fixture Test"]);
        std::fs::write(dir.join("README.md"), "fixture\n").expect("readme");
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "chore: initial"]);
        temp
    }

    fn component_at(path: &Path) -> Component {
        Component {
            local_path: path.to_string_lossy().to_string(),
            ..Component::default()
        }
    }

    #[test]
    fn fresh_build_records_commit_clean_tree_and_artifact_identity() {
        let temp = committed_repo();
        let dir = temp.path();
        let artifact = dir.join("plugin.zip");
        std::fs::write(&artifact, b"artifact-bytes").expect("artifact");

        let provenance = capture_build_provenance(
            &component_at(dir),
            BuildSource::FreshBuild,
            true,
            Some(artifact.as_path()),
        );

        assert_eq!(provenance.source, BuildSource::FreshBuild);
        assert!(provenance.build_ran);
        assert!(provenance.built_from_ref.is_none());
        assert_eq!(
            provenance.built_from_commit.as_deref().map(str::len),
            Some(40)
        );
        assert_eq!(provenance.working_tree_dirty, Some(false));

        let identity = provenance.artifact_identity.expect("artifact identity");
        assert_eq!(identity.size_bytes, Some(b"artifact-bytes".len() as u64));
        // SHA-256 of the artifact bytes is reported so stale artifacts are detectable.
        assert_eq!(
            identity.sha256.as_deref(),
            Some("6521df166eb07efaf36eba5b6bedefd9d6a252e9c80bab1c99653700ec71473c"),
            "sha256 should be the lowercase hex digest of the artifact contents"
        );
    }

    #[test]
    fn fresh_build_flags_dirty_working_tree() {
        let temp = committed_repo();
        let dir = temp.path();
        std::fs::write(dir.join("README.md"), "modified\n").expect("modify tracked file");

        let provenance =
            capture_build_provenance(&component_at(dir), BuildSource::FreshBuild, true, None);

        assert_eq!(provenance.working_tree_dirty, Some(true));
        assert!(provenance.artifact_identity.is_none());
    }

    #[test]
    fn downloaded_release_skips_local_dirtiness_and_reports_not_built() {
        let temp = committed_repo();
        let dir = temp.path();
        std::fs::write(dir.join("README.md"), "modified\n").expect("modify tracked file");

        let provenance = capture_build_provenance(
            &component_at(dir),
            BuildSource::DownloadedRelease,
            false,
            None,
        );

        assert_eq!(provenance.source, BuildSource::DownloadedRelease);
        assert!(!provenance.build_ran);
        // Local tree dirtiness is not meaningful provenance for a downloaded asset.
        assert_eq!(provenance.working_tree_dirty, None);
    }

    #[test]
    fn reused_artifact_reports_no_build_ran() {
        let temp = committed_repo();
        let dir = temp.path();
        let artifact = dir.join("plugin.zip");
        std::fs::write(&artifact, b"reused").expect("artifact");

        let provenance = capture_build_provenance(
            &component_at(dir),
            BuildSource::ReusedArtifact,
            false,
            Some(artifact.as_path()),
        );

        assert_eq!(provenance.source, BuildSource::ReusedArtifact);
        assert!(!provenance.build_ran);
        assert!(provenance.artifact_identity.is_some());
    }
}
