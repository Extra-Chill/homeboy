//! Live provider-activity sampling for a running Cook.
//!
//! A Cook's durable lifecycle record answers "what did this run do?". Until
//! this module existed nothing answered "what is the provider doing *right
//! now?*" — heartbeats were byte-identical and `agent-task status` described the
//! run's budget, liveness and candidate scan, none of which distinguishes an
//! agent that is editing from one that has spent six minutes compiling. The
//! only way to tell them apart was `ps aux | grep` plus `git status` on a path
//! the operator had to already know (#11482).
//!
//! Two independent signals answer it, and they are deliberately ordered:
//!
//! 1. **Edit count in the destination worktree.** "Zero files written after N
//!    minutes" is the single most actionable thing an operator can learn about
//!    a running cook, and unlike a process sample it is true regardless of what
//!    the provider claims to be doing.
//! 2. **The provider's current shell command and its age**, sampled from the
//!    process tree.
//!
//! Both are diagnostics, so both fail soft: an unreadable worktree or an
//! unavailable `ps` yields an absent field, never an error that can stop a
//! cook. Sampling is bounded — one `git status`, one `ps`, one truncated
//! command — because a diagnostic that scales with the work it observes is how
//! observability becomes the outage.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Instant;

use homeboy_core::process_activity::{self, DescendantActivity};

/// A single sample of what a running Cook's provider is doing.
///
/// Every field is optional: this is evidence, and absent evidence is reported
/// as absent rather than as a zero that reads like a measurement.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookProviderActivity {
    /// Destination worktree the provider is expected to write into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<String>,
    /// Files with uncommitted changes in that worktree, untracked included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<usize>,
    /// Commits made in that worktree since the provider started working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits_written: Option<usize>,
    /// The provider's current command line, truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_pid: Option<u32>,
    /// How long that command has been running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_elapsed_seconds: Option<u64>,
    /// Seconds since provider execution began, i.e. elapsed time in the
    /// current activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
}

impl CookProviderActivity {
    /// True when the sample carries nothing an operator could act on.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// One bounded line describing the sample, for foreground progress output.
    ///
    /// Written so the two facts that decide "should I intervene?" — how much
    /// has been written, and what has been running how long — read together:
    /// "no files written yet, 6m12s in the build command it spawned, 6m40s
    /// elapsed".
    pub fn summary_line(&self) -> Option<String> {
        let mut parts = Vec::new();
        match self.files_changed {
            Some(0) => parts.push(match self.commits_written {
                Some(commits) if commits > 0 => format!("{commits} commit(s), no pending edits"),
                _ => "no files written yet".to_string(),
            }),
            Some(count) => {
                let mut written = format!("{count} file(s) changed");
                if let Some(commits) = self.commits_written.filter(|commits| *commits > 0) {
                    written.push_str(&format!(", {commits} commit(s)"));
                }
                parts.push(written);
            }
            None => {}
        }
        if let Some(command) = self.command.as_deref() {
            parts.push(match self.command_elapsed_seconds {
                Some(seconds) => format!("{} in `{command}`", format_duration(seconds)),
                None => format!("running `{command}`"),
            });
        }
        if let Some(seconds) = self.elapsed_seconds {
            parts.push(format!("{} elapsed", format_duration(seconds)));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    /// Durable projection for the lifecycle record.
    pub fn to_record_value(&self) -> Option<Value> {
        (!self.is_empty()).then(|| serde_json::to_value(self).unwrap_or(Value::Null))
    }
}

/// Render seconds the way an operator reads them off a stalled cook.
pub fn format_duration(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

/// Samples provider activity for one provider execution.
///
/// The probe is constructed *before* the provider runs so "commits written"
/// is measured against the tree the provider inherited, not against whatever
/// base a reader guesses later. A follow-up attempt starts from a baseline
/// worktree whose HEAD is not the cook's original base, so an inferred base
/// would silently report a previous attempt's commits as this one's progress.
pub struct CookActivityProbe {
    worktree_root: Option<PathBuf>,
    start_head: Option<String>,
    started_at: Instant,
}

impl CookActivityProbe {
    pub fn new(worktree_root: Option<PathBuf>) -> Self {
        let start_head = worktree_root.as_deref().and_then(git_head_sha);
        Self {
            worktree_root,
            start_head,
            started_at: Instant::now(),
        }
    }

    /// Take one bounded sample. Never fails: an unreadable worktree or an
    /// unavailable process table narrows the sample, it does not end the cook.
    pub fn sample(&self, owner_pid: u32) -> CookProviderActivity {
        let mut activity = CookProviderActivity {
            elapsed_seconds: Some(self.started_at.elapsed().as_secs()),
            ..Default::default()
        };
        if let Some(root) = self.worktree_root.as_deref() {
            activity.worktree_root = Some(root.display().to_string());
            activity.files_changed = worktree_files_changed(root);
            activity.commits_written = self
                .start_head
                .as_deref()
                .and_then(|base| commits_since(root, base));
        }
        if let Some(process) = process_activity::descendant_activity(owner_pid) {
            let DescendantActivity {
                pid,
                elapsed_seconds,
                command,
                ..
            } = process;
            activity.command = Some(command);
            activity.command_pid = Some(pid);
            activity.command_elapsed_seconds = Some(elapsed_seconds);
        }
        activity
    }
}

/// Count files the provider has written but not committed, untracked included.
///
/// Untracked files count because a provider that has created new files has
/// unambiguously done work; excluding them would report a scaffolding cook as
/// having produced nothing.
pub fn worktree_files_changed(root: &Path) -> Option<usize> {
    let porcelain = homeboy_core::git::output_allow_empty(
        root,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    Some(count_porcelain_entries(&porcelain))
}

/// Count entries in `git status --porcelain` output.
///
/// Homeboy's own run state lives under `.homeboy/` inside a cook worktree and
/// is written by the controller, not the provider. Counting it would report
/// edits on a cook where the provider has written nothing at all — precisely
/// the signal this exists to make trustworthy.
pub fn count_porcelain_entries(porcelain: &str) -> usize {
    porcelain
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !is_homeboy_internal_entry(line))
        .count()
}

fn is_homeboy_internal_entry(line: &str) -> bool {
    // Porcelain v1 status lines are `XY <path>`; a rename is `XY <old> -> <new>`
    // and the destination is what matters.
    let path = line.get(3..).unwrap_or("").trim();
    let path = path.rsplit(" -> ").next().unwrap_or(path).trim_matches('"');
    path == ".homeboy" || path.starts_with(".homeboy/")
}

/// Count commits made in `root` since `base_sha`.
fn commits_since(root: &Path, base_sha: &str) -> Option<usize> {
    let range = format!("{base_sha}..HEAD");
    homeboy_core::git::output_allow_empty(root, &["rev-list", "--count", range.as_str()])
        .and_then(|count| count.trim().parse().ok())
}

fn git_head_sha(root: &Path) -> Option<String> {
    homeboy_core::git::output_allow_empty(root, &["rev-parse", "HEAD"])
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_provider_written_files_but_not_homeboy_run_state() {
        // Controller-written `.homeboy/` state must never read as provider
        // progress: a cook where the agent wrote nothing has to report zero.
        let porcelain = concat!(
            " M crates/homeboy-agents/src/lib.rs\n",
            "?? crates/homeboy-agents/src/new_module.rs\n",
            "?? .homeboy/run/state.json\n",
            " M .homeboy\n",
            "R  docs/old.md -> docs/new.md\n",
            "\n",
        );

        assert_eq!(count_porcelain_entries(porcelain), 3);
    }

    #[test]
    fn a_clean_worktree_counts_zero_rather_than_reporting_nothing() {
        // `Some(0)` and `None` mean different things to an operator: "the agent
        // has written nothing" versus "we could not look".
        assert_eq!(count_porcelain_entries(""), 0);
    }

    #[test]
    fn zero_edits_is_the_headline_of_the_summary_line() {
        let activity = CookProviderActivity {
            files_changed: Some(0),
            command: Some("cargo test -q -p homeboy-agents".to_string()),
            command_elapsed_seconds: Some(372),
            elapsed_seconds: Some(400),
            ..Default::default()
        };

        let summary = activity.summary_line().expect("activity renders");

        assert!(summary.starts_with("no files written yet"));
        assert!(summary.contains("6m12s in `cargo test -q -p homeboy-agents`"));
        assert!(summary.contains("6m40s elapsed"));
    }

    #[test]
    fn committed_work_is_not_reported_as_no_progress() {
        // A provider that committed leaves a clean tree. Reporting that as
        // "no files written yet" would send the operator to kill a working cook.
        let activity = CookProviderActivity {
            files_changed: Some(0),
            commits_written: Some(2),
            ..Default::default()
        };

        let summary = activity.summary_line().expect("activity renders");

        assert!(summary.contains("2 commit(s)"));
        assert!(!summary.contains("no files written yet"));
    }

    #[test]
    fn an_empty_sample_renders_nothing_rather_than_an_empty_line() {
        let activity = CookProviderActivity::default();

        assert!(activity.is_empty());
        assert_eq!(activity.summary_line(), None);
        assert_eq!(activity.to_record_value(), None);
    }

    #[test]
    fn durations_read_the_way_an_operator_reads_them() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59), "59s");
        assert_eq!(format_duration(60), "1m00s");
        assert_eq!(format_duration(372), "6m12s");
        assert_eq!(format_duration(3_600), "1h00m");
        assert_eq!(format_duration(7_845), "2h10m");
    }

    #[test]
    fn a_probe_without_a_worktree_still_reports_elapsed_time() {
        let probe = CookActivityProbe::new(None);

        let activity = probe.sample(std::process::id());

        assert!(activity.elapsed_seconds.is_some());
        assert_eq!(activity.worktree_root, None);
        assert_eq!(activity.files_changed, None);
    }

    #[test]
    fn worktree_edit_count_measures_a_real_repository_and_ignores_run_state() {
        let temp = tempfile::tempdir().expect("probe fixture directory");
        let root = temp.path();
        let initialized = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .expect("git init runs");
        assert!(initialized.success(), "git init succeeds");

        // A cook that has produced nothing reports zero, not "unknown".
        assert_eq!(worktree_files_changed(root), Some(0));

        std::fs::write(root.join("edited.rs"), "fn main() {}\n").expect("write provider edit");
        assert_eq!(worktree_files_changed(root), Some(1));

        // Controller-owned run state is not provider progress.
        std::fs::create_dir_all(root.join(".homeboy/run")).expect("create run state directory");
        std::fs::write(root.join(".homeboy/run/state.json"), "{}\n").expect("write run state");
        assert_eq!(worktree_files_changed(root), Some(1));
    }

    #[test]
    fn activity_round_trips_through_its_durable_projection() {
        let activity = CookProviderActivity {
            worktree_root: Some("/tmp/wt".to_string()),
            files_changed: Some(3),
            commits_written: Some(1),
            command: Some("cargo test".to_string()),
            command_pid: Some(4242),
            command_elapsed_seconds: Some(12),
            elapsed_seconds: Some(30),
        };

        let value = activity.to_record_value().expect("non-empty activity");
        let restored: CookProviderActivity =
            serde_json::from_value(value).expect("durable projection round-trips");

        assert_eq!(restored, activity);
    }
}
