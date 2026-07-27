//! Read-only git query primitives: resolving refs, reading HEAD SHAs, porcelain
//! status, remotes, and repository roots.
//!
//! Split out of `primitives.rs` to keep mutating operations (clone, stage,
//! commit, branch updates) separate from pure reads.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::engine::command as engine_command;

/// Default deadline for read-only git probes issued by catalog/discovery code
/// paths.
///
/// A diagnostic read must answer quickly. `git rev-parse` looks cheap but can
/// block indefinitely on a wedged filesystem, a stuck index lock, a hanging
/// fsmonitor, or a credential helper, which turned provider-catalog discovery
/// into an unbounded hang (#9763). Discovery callers use the bounded probes
/// below so a stuck repository degrades to a labelled partial instead.
pub const DEFAULT_GIT_READ_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of a bounded read-only git probe.
///
/// `Unresolved` and `TimedOut` are deliberately distinct: the first is a normal
/// negative answer (not a repo, command failed, empty output), the second is a
/// missing answer the caller must label rather than silently report as absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedGitRead {
    /// The probe completed with trimmed, non-empty stdout.
    Resolved(String),
    /// The probe completed without a usable answer.
    Unresolved,
    /// The probe exceeded its deadline; its process group was terminated.
    TimedOut,
}

impl BoundedGitRead {
    /// The resolved value, or `None` for both negative outcomes.
    pub fn resolved(self) -> Option<String> {
        match self {
            Self::Resolved(value) => Some(value),
            Self::Unresolved | Self::TimedOut => None,
        }
    }

    /// Whether the probe ran out of budget rather than answering.
    pub fn timed_out(&self) -> bool {
        matches!(self, Self::TimedOut)
    }
}

/// Run a read-only git command under a deadline, terminating its isolated
/// process group on expiry.
///
/// Mirrors [`output_optional`] but never blocks past `timeout`.
pub fn output_optional_within(git_root: &Path, args: &[&str], timeout: Duration) -> BoundedGitRead {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(git_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    engine_command::isolate_process_tree(&mut command);
    let Ok(mut child) = command.spawn() else {
        return BoundedGitRead::Unresolved;
    };
    let started = Instant::now();
    let mut timed_out = false;
    let waited = engine_command::wait_with_bounded_output_until_cancelled(
        &mut child,
        engine_command::DEFAULT_CAPTURE_LIMIT_BYTES,
        || {
            timed_out = started.elapsed() >= timeout;
            timed_out
        },
    );
    if timed_out {
        return BoundedGitRead::TimedOut;
    }
    let Ok(output) = waited else {
        return BoundedGitRead::Unresolved;
    };
    let output = output.into_output();
    if !output.status.success() {
        return BoundedGitRead::Unresolved;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        BoundedGitRead::Unresolved
    } else {
        BoundedGitRead::Resolved(value)
    }
}

/// Get the full HEAD commit SHA under a deadline.
pub fn head_sha_within(git_root: &Path, timeout: Duration) -> BoundedGitRead {
    output_optional_within(git_root, &["rev-parse", "HEAD"], timeout)
}

/// Resolve a git revision to its commit/object id, returning None when the ref
/// cannot be resolved.
pub fn rev_parse(git_root: &Path, git_ref: &str) -> Option<String> {
    output_optional(git_root, &["rev-parse", git_ref])
}

/// Run a git command and return stdout bytes when the command succeeds.
pub fn output_optional_bytes(git_root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(git_root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    output.status.success().then_some(output.stdout)
}

/// Run a git command and return trimmed stdout when the command succeeds and is non-empty.
pub fn output_optional(git_root: &Path, args: &[&str]) -> Option<String> {
    let output = output_optional_bytes(git_root, args)?;
    let value = String::from_utf8_lossy(&output).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Run a git command (via `git -C <path>`) and return trimmed stdout on success,
/// *including* the empty string.
///
/// Unlike [`output_optional`], a successful command that produces no output
/// yields `Some("")` rather than `None` — empty output is a valid result for
/// commands like `git status --porcelain` on a clean tree, and some callers
/// need to distinguish "ran clean" (`Some("")`) from "failed" (`None`). Callers
/// that require non-empty output validate it themselves.
pub fn output_allow_empty(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Get the full HEAD commit SHA from a git directory.
pub fn head_sha(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["rev-parse", "HEAD"])
}

/// Get the short HEAD commit SHA from a git directory.
pub fn head_sha_short(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["rev-parse", "--short", "HEAD"])
}

/// Get porcelain status bytes from a git directory.
pub fn status_porcelain_bytes(git_root: &Path) -> Option<Vec<u8>> {
    output_optional_bytes(git_root, &["status", "--porcelain=v1", "-z"])
}

/// Get porcelain status text from a git directory.
pub fn status_porcelain(git_root: &Path) -> Option<String> {
    output_optional_bytes(git_root, &["status", "--porcelain=v1"])
        .map(|output| String::from_utf8_lossy(&output).to_string())
}

/// Get a remote URL from a git directory.
pub fn remote_url(git_root: &Path, remote: &str) -> Option<String> {
    output_optional(git_root, &["remote", "get-url", remote])
}

/// Get the git repository root directory from any path within the repo.
pub fn toplevel(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["rev-parse", "--show-toplevel"])
}

/// Get the git repository root directory from any path within the repo.
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    toplevel(path).map(PathBuf::from)
}

pub fn current_branch(git_root: &Path) -> Option<String> {
    output_optional(git_root, &["branch", "--show-current"])
}

pub fn remote_origin_url(git_root: &Path) -> Option<String> {
    remote_url(git_root, "origin")
}

/// Get the short HEAD revision from a git directory.
pub fn short_head_revision(dir: &Path) -> Option<String> {
    head_sha_short(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run git test fixture command");

        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn optional_helpers_return_head_remote_toplevel_and_clean_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "--quiet"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "https://example.test/repo.git"],
        );
        std::fs::write(dir.path().join("README.md"), "hello\n").expect("write fixture file");
        git(dir.path(), &["add", "README.md"]);
        git(
            dir.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "initial",
            ],
        );

        assert_eq!(
            Path::new(&toplevel(dir.path()).expect("git toplevel"))
                .canonicalize()
                .expect("canonical git toplevel"),
            dir.path().canonicalize().expect("canonical fixture dir")
        );
        assert_eq!(
            remote_url(dir.path(), "origin").as_deref(),
            Some("https://example.test/repo.git")
        );
        assert!(head_sha(dir.path()).is_some());
        assert!(head_sha_short(dir.path()).is_some());
        assert_eq!(status_porcelain(dir.path()).as_deref(), Some(""));
        assert_eq!(
            status_porcelain_bytes(dir.path()).as_deref(),
            Some(&b""[..])
        );

        assert_eq!(
            head_sha_within(dir.path(), DEFAULT_GIT_READ_PROBE_TIMEOUT).resolved(),
            head_sha(dir.path()),
            "the bounded probe must agree with the unbounded one when git answers in time"
        );
    }

    #[test]
    fn bounded_probe_reports_unresolved_outside_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert_eq!(
            head_sha_within(dir.path(), DEFAULT_GIT_READ_PROBE_TIMEOUT),
            BoundedGitRead::Unresolved
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_probe_times_out_instead_of_hanging() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "--quiet"]);

        // A shell alias that outlives the budget is the deterministic stand-in
        // for a wedged repository. The probe must terminate the child process
        // group and return a labelled `TimedOut` rather than block, and that
        // outcome must never be mistaken for "no revision" (#9763).
        let started = Instant::now();
        let probe = output_optional_within(
            dir.path(),
            &["-c", "alias.hbprobehang=!sleep 30", "hbprobehang"],
            Duration::from_millis(200),
        );

        assert!(
            probe.timed_out(),
            "expected a timed-out probe, got {probe:?}"
        );
        assert_ne!(probe, BoundedGitRead::Unresolved);
        assert_eq!(probe.resolved(), None);
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "bounded probe must not wait for the wedged child"
        );
    }
}
