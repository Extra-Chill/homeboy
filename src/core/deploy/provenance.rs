//! Build provenance tracking.
//!
//! Provides tag-gap detection so `build` and `deploy` can warn about
//! unreleased commits ahead of the latest tag.

use crate::component::Component;
use crate::engine::command;
use crate::git;

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
