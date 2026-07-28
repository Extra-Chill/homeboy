//! Controller-side git ancestry probe for runner recovery guidance (#10525).
//!
//! `runner status` prefers a verified newer runner build over an older
//! controller revision when regenerating a `refresh-homeboy` command, and it
//! decides that with `git merge-base --is-ancestor`. That probe reads the
//! caller's *ambient* working directory: it is meaningful when the controller
//! runs inside a Homeboy checkout and meaningless everywhere else.
//!
//! Two things were wrong with running it as a bare `Command::status()`:
//!
//! 1. The child inherited stderr, so invoking `homeboy runner status <id> --full`
//!    from a non-checkout printed `fatal: not a git repository (or any of the
//!    parent directories): .git` — twice — *before* the structured
//!    `homeboy/command-result/v3` envelope on stdout. An orchestration client
//!    then has to separate real runner failure from unrelated ambient noise.
//! 2. `.is_ok_and(|status| status.success())` collapsed "not an ancestor",
//!    "not a checkout", and "git is not installed" into one `false`. Homeboy has
//!    repeatedly lost diagnostics that way (see #10576, where a release failure
//!    reported only "Unknown error"), so the cause must survive even though the
//!    answer is the same fallback in every case.
//!
//! This module captures the child's streams and reports an unusable probe as a
//! bounded typed degradation on the existing read-only probe ledger, which
//! `runner status` already drains into `probe_degradations`. A plain
//! non-ancestor answer — exit 1 with no stderr — is a real answer and records
//! nothing.

use std::process::Command;

use homeboy::runner::readonly_probe::{self, ReadOnlyProbeDegradation, REASON_PROBE_UNAVAILABLE};

/// Stable probe label reported in `probe_degradations`.
const PROBE: &str = "controller_git_ancestry";

/// Upper bound on how much of git's stderr is echoed into the degradation.
/// The diagnostic must stay a bounded field in a machine-readable envelope, not
/// an unbounded subprocess transcript.
const MAX_DETAIL_CHARS: usize = 400;

/// Is `older` an ancestor of `newer` in the controller's ambient checkout?
///
/// Returns `false` when the question cannot be answered, which is the same
/// conservative answer as "no": the caller falls back to the controller's own
/// refresh ref. When the probe could not run at all, the reason is recorded as
/// a degradation rather than printed or discarded.
pub(super) fn commits_are_ancestral(older: &str, newer: &str) -> bool {
    // `output()` captures both child streams. `status()` would inherit them and
    // print git's `fatal:` line straight past the structured envelope.
    ancestry_from_probe(
        Command::new("git")
            .args(["merge-base", "--is-ancestor", older, newer])
            .output(),
    )
}

/// Classify a completed probe. Split from the spawn so the "answered false"
/// versus "could not answer" distinction is deterministically testable without
/// depending on the machine's git or working directory.
fn ancestry_from_probe(probe: std::io::Result<std::process::Output>) -> bool {
    match probe {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            // A non-zero exit with nothing on stderr is git answering the
            // question: `older` is not an ancestor of `newer`. Only a
            // diagnostic on stderr means the probe itself could not run.
            if !stderr.is_empty() {
                record_unavailable(stderr);
            }
            false
        }
        Err(error) => {
            record_unavailable(&format!("git could not be executed: {error}"));
            false
        }
    }
}

fn record_unavailable(git_diagnostic: &str) {
    readonly_probe::record_degradation(ReadOnlyProbeDegradation {
        probe: PROBE.to_string(),
        runner_id: None,
        reason_code: REASON_PROBE_UNAVAILABLE,
        timeout_seconds: 0,
        detail: unavailable_detail(git_diagnostic),
    });
}

/// Split from [`record_unavailable`] so the operator-facing wording is
/// assertable without spawning git or touching the thread-local ledger.
fn unavailable_detail(git_diagnostic: &str) -> String {
    format!(
        "controller git ancestry probe could not run in the current directory, \
         so runner recovery guidance falls back to the controller's own build \
         ref instead of preferring a verified newer runner build. Run from a \
         Homeboy checkout to restore it. git reported: {}",
        bounded(git_diagnostic)
    )
}

fn bounded(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_DETAIL_CHARS {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(MAX_DETAIL_CHARS).collect();
    format!("{truncated}… (truncated)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Output;

    #[cfg(unix)]
    fn exit(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }

    #[cfg(not(unix))]
    fn exit(code: i32) -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    }

    fn probe(code: i32, stderr: &str) -> std::io::Result<Output> {
        Ok(Output {
            status: exit(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    /// The exact shape reported in #10525: `runner status --full` invoked from a
    /// directory that is not a checkout. The answer is the conservative
    /// fallback, git's line never reaches a stream, and the cause survives as a
    /// typed degradation on the read-only probe ledger.
    #[test]
    fn a_non_checkout_working_directory_reports_a_bounded_degradation_not_raw_git_stderr() {
        readonly_probe::take_degradations();

        let answer = ancestry_from_probe(probe(
            128,
            "fatal: not a git repository (or any of the parent directories): .git\n",
        ));

        assert!(
            !answer,
            "an unanswerable ancestry probe must not claim ancestry"
        );
        let degradations = readonly_probe::take_degradations();
        assert_eq!(degradations.len(), 1);
        assert_eq!(degradations[0].probe, PROBE);
        assert_eq!(degradations[0].reason_code, REASON_PROBE_UNAVAILABLE);
        assert!(
            degradations[0]
                .detail
                .contains("fatal: not a git repository"),
            "the underlying git diagnostic must survive: {}",
            degradations[0].detail
        );
    }

    /// `git merge-base --is-ancestor` reports a plain "no" as exit 1 with no
    /// stderr. That is an answer, not a degradation, and must not pollute the
    /// ledger of a healthy status call.
    #[test]
    fn a_plain_non_ancestor_answer_records_nothing() {
        readonly_probe::take_degradations();

        assert!(!ancestry_from_probe(probe(1, "")));
        assert!(readonly_probe::take_degradations().is_empty());

        assert!(ancestry_from_probe(probe(0, "")));
        assert!(readonly_probe::take_degradations().is_empty());
    }

    #[test]
    fn a_missing_git_binary_is_reported_rather_than_swallowed() {
        readonly_probe::take_degradations();

        assert!(!ancestry_from_probe(Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no such file or directory",
        ))));

        let degradations = readonly_probe::take_degradations();
        assert_eq!(degradations.len(), 1);
        assert!(degradations[0].detail.contains("git could not be executed"));
    }

    #[test]
    fn the_reported_detail_stays_bounded() {
        let long = unavailable_detail(&"x".repeat(MAX_DETAIL_CHARS * 3));

        assert!(long.contains("(truncated)"));
        assert!(long.chars().count() < MAX_DETAIL_CHARS * 2);
    }

    /// Structural guard for the leak itself: `status()` inherits the parent's
    /// stdout/stderr, which is how git's `fatal:` line got in front of the
    /// `homeboy/command-result/v3` envelope. This probe must always capture.
    #[test]
    fn the_probe_never_inherits_the_parent_streams() {
        let source = include_str!("controller_ancestry.rs");
        let start = source
            .find("pub(super) fn commits_are_ancestral")
            .expect("probe entry point");
        let end = source[start..]
            .find("fn ancestry_from_probe")
            .expect("next item")
            + start;
        let spawn = &source[start..end];
        // Composed rather than written literally so this assertion does not
        // match itself when it scans its own file.
        let inheriting_spawn = format!(".{}()", "status");

        assert!(spawn.contains(".output()"));
        assert!(
            !spawn.contains(&inheriting_spawn),
            "the ancestry probe must capture git's streams, never inherit them"
        );
    }
}
