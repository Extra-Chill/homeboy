//! Build provenance tracking.
//!
//! Records metadata about what was built so `deploy` can make informed
//! decisions about whether to reuse an existing artifact or rebuild
//! from the latest tag.
//!
//! The sidecar lives at `.homeboy-build-meta.json` in the component's
//! local directory root.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::component::Component;
use crate::engine::command;
use crate::git;

const META_FILENAME: &str = ".homeboy-build-meta.json";

/// Metadata recorded after a successful build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildProvenance {
    /// Full commit hash that was built
    pub commit: String,
    /// Branch or ref name (e.g. "main", or tag name if detached at a tag)
    pub git_ref: String,
    /// Latest tag at build time (if any)
    pub tag: Option<String>,
    /// Number of commits HEAD is ahead of the latest tag
    pub ahead_of_tag: u32,
    /// ISO 8601 timestamp of the build
    pub timestamp: String,
    /// Whether the working tree had uncommitted changes at build time
    pub dirty: bool,
}

impl BuildProvenance {
    /// Returns true if this build was from the exact tagged commit (not ahead).
    pub(crate) fn is_tagged_build(&self) -> bool {
        self.ahead_of_tag == 0 && self.tag.is_some() && !self.dirty
    }

    /// Returns true if this build includes unreleased commits beyond the tag.
    pub fn is_ahead_of_tag(&self) -> bool {
        self.ahead_of_tag > 0
    }
}

/// Capture build provenance for a component's current git state.
pub fn capture(component: &Component) -> Option<BuildProvenance> {
    let path = &component.local_path;

    let commit = command::run_in_optional(path, "git", &["rev-parse", "HEAD"])?;

    let git_ref = command::run_in_optional(path, "git", &["symbolic-ref", "--short", "HEAD"])
        .unwrap_or_else(|| {
            // Detached HEAD — use the commit hash as ref
            commit.chars().take(12).collect()
        });

    let tag = git::get_latest_tag(path).ok().flatten();

    let ahead_of_tag = tag
        .as_ref()
        .and_then(|t| {
            command::run_in_optional(
                path,
                "git",
                &["rev-list", "--count", &format!("{}..HEAD", t)],
            )
        })
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);

    let dirty = !git::is_workdir_clean(Path::new(path));

    let timestamp = chrono_now_iso();

    Some(BuildProvenance {
        commit,
        git_ref,
        tag,
        ahead_of_tag,
        timestamp,
        dirty,
    })
}

/// Write build provenance to the sidecar file in the component directory.
pub fn write(component: &Component, provenance: &BuildProvenance) -> std::io::Result<()> {
    let meta_path = meta_path(&component.local_path);
    let json = serde_json::to_string_pretty(provenance).map_err(std::io::Error::other)?;
    std::fs::write(&meta_path, json)
}

/// Read build provenance from the sidecar file, if it exists.
pub fn read(component: &Component) -> Option<BuildProvenance> {
    read_from_path(&component.local_path)
}

/// Read build provenance from a path (component local_path).
pub(crate) fn read_from_path(local_path: &str) -> Option<BuildProvenance> {
    let meta_path = meta_path(local_path);
    let content = std::fs::read_to_string(&meta_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Check if the provenance is still valid for the current HEAD.
///
/// Returns true if the recorded commit matches the current HEAD,
/// meaning the build artifact corresponds to the checked-out code.
pub fn is_current(component: &Component, provenance: &BuildProvenance) -> bool {
    let current_commit =
        command::run_in_optional(&component.local_path, "git", &["rev-parse", "HEAD"]);
    current_commit.as_deref() == Some(provenance.commit.as_str())
}

fn meta_path(local_path: &str) -> PathBuf {
    Path::new(local_path).join(META_FILENAME)
}

/// ISO 8601 UTC timestamp without external crate dependency.
fn chrono_now_iso() -> String {
    // Use `date` command for UTC ISO format — avoids adding chrono crate
    command::run_in_optional(".", "date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .unwrap_or_else(|| "unknown".to_string())
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

/// Log a warning about the HEAD-vs-tag gap for a component.
/// Format a tag gap as a human-readable warning string.
///
/// Used by both `build` and `deploy` commands.
pub(crate) fn format_tag_gap(component_id: &str, gap: &TagGap, context: &str) -> String {
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
mod tests {
    use super::*;

    #[test]
    fn test_is_tagged_build() {
        let p = BuildProvenance {
            commit: "abc123".to_string(),
            git_ref: "main".to_string(),
            tag: Some("v1.0.0".to_string()),
            ahead_of_tag: 0,
            timestamp: "2026-03-28T20:00:00Z".to_string(),
            dirty: false,
        };
        assert!(p.is_tagged_build());
    }

    #[test]
    fn test_is_tagged_build_dirty() {
        let p = BuildProvenance {
            commit: "abc123".to_string(),
            git_ref: "main".to_string(),
            tag: Some("v1.0.0".to_string()),
            ahead_of_tag: 0,
            timestamp: "2026-03-28T20:00:00Z".to_string(),
            dirty: true,
        };
        assert!(!p.is_tagged_build());
    }

    #[test]
    fn test_is_ahead_of_tag() {
        let p = BuildProvenance {
            commit: "abc123".to_string(),
            git_ref: "main".to_string(),
            tag: Some("v1.0.0".to_string()),
            ahead_of_tag: 5,
            timestamp: "2026-03-28T20:00:00Z".to_string(),
            dirty: false,
        };
        assert!(p.is_ahead_of_tag());
        assert!(!p.is_tagged_build());
    }
}
