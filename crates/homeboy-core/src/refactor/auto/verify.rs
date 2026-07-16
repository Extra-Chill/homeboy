//! Post-write verify gate for autofix (#1167).
//!
//! After `apply_fixes_via_edit_ops` writes auto-apply edits to disk, the
//! verify gate runs a caller-provided command from the component root. A
//! non-zero exit code (or timeout) triggers a full revert of every file the
//! apply phase touched, and every applied chunk is reclassified as
//! `Reverted` with the verify output attached.
//!
//! This is a **tool-level** safety net, layered below per-rule rails. Rules
//! still own their own "is this fix safe?" checks (see #1166); verify catches
//! the class of bug a rule's rails miss — e.g. a subtle boundary-depth
//! mistake that ships compile-broken code despite passing the rule's
//! internal-balance check.
//!
//! ## Contract
//!
//! 1. Caller captures the pre-apply content of every file the apply phase is
//!    about to write (see `capture_pre_apply_snapshot`).
//! 2. Apply phase runs; any files that were actually modified are recorded on
//!    the returned `ApplyChunkResult`s.
//! 3. Caller passes the snapshot + chunk results + verify config to
//!    `run_verify_gate`. If verify fails, files are reverted in place and
//!    chunk results are rewritten to `Reverted`. Return carries the verify
//!    stdout/stderr so the operator can see exactly what broke.
//!
//! ## Env gating
//!
//! - `HOMEBOY_AUTOFIX_VERIFY=0` disables the gate even when configured.
//! - `HOMEBOY_AUTOFIX_VERIFY=1` (currently a no-op; reserved for future
//!   forced-on semantics once a default verify command can be derived from
//!   the extension id).
//!
//! Default: gate runs when the extension provides an `autofix_verify` config.

use crate::engine::undo::InMemoryRollback;
use crate::extension::AutofixVerifyConfig;
use crate::refactor::auto::{ApplyChunkResult, ChunkStatus};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Env var that disables verify even when a config is present.
pub const VERIFY_ENV_VAR: &str = "HOMEBOY_AUTOFIX_VERIFY";

/// Outcome of a single verify-gate execution.
#[derive(Debug)]
pub struct VerifyOutcome {
    /// True when verify exited 0 within the timeout.
    pub passed: bool,
    /// Exit code when the process ran to completion. `None` on timeout / spawn
    /// failure — both treated as failure.
    pub exit_code: Option<i32>,
    /// Combined stdout + stderr, truncated to ~4KB to keep chunk results
    /// small. Used for the `error` field on reverted chunks.
    pub combined_output: String,
    /// Duration the verify ran before completing / timing out.
    pub duration: Duration,
    /// True when verify was skipped because of the env gate or a missing
    /// config. When skipped, `passed` is also true (no-op).
    pub skipped: bool,
    /// Human-friendly reason string used in logs.
    pub reason: &'static str,
}

impl VerifyOutcome {
    fn skipped(reason: &'static str) -> Self {
        Self {
            passed: true,
            exit_code: None,
            combined_output: String::new(),
            duration: Duration::from_secs(0),
            skipped: true,
            reason,
        }
    }
}

/// Capture the pre-apply snapshot of every file the apply phase plans to
/// touch. Pass this to `run_verify_gate` after the apply phase returns.
///
/// `files` are expected to be relative to `root` (matching the `ApplyChunkResult`
/// shape). Non-existent files are recorded as "created" — revert will delete
/// them on failure.
pub fn capture_pre_apply_snapshot<I, P>(root: &Path, files: I) -> InMemoryRollback
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut rollback = InMemoryRollback::new();
    for file in files {
        let abs = root.join(file.as_ref());
        rollback.capture(&abs);
    }
    rollback
}

/// Read the env gate. Returns `Some(false)` only when explicitly disabled.
/// Absence or unparseable values fall back to the default (enabled).
fn env_gate_enabled() -> bool {
    match std::env::var(VERIFY_ENV_VAR) {
        Ok(v) => !matches!(v.trim(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    }
}

/// Run the verify command and, on failure, revert every file the apply phase
/// captured and reclassify applied chunks as reverted.
///
/// The caller passes:
/// - `config`: the extension's verify config (or `None` to skip the gate).
/// - `rollback`: the pre-apply snapshot.
/// - `root`: component root (verify CWD).
/// - `chunk_results`: the mutable apply-phase results. On verify failure,
///   `Applied` chunks are rewritten in place to `Reverted` with the verify
///   output attached.
///
/// Returns the `VerifyOutcome` so callers can log / surface it to the
/// operator. When the gate is skipped (no config, env disabled, or no files
/// were actually modified), the outcome is `passed=true, skipped=true` and
/// nothing is touched on disk.
pub fn run_verify_gate(
    config: Option<&AutofixVerifyConfig>,
    rollback: &InMemoryRollback,
    root: &Path,
    chunk_results: &mut [ApplyChunkResult],
) -> VerifyOutcome {
    // Short-circuit: nothing to verify if no files were applied.
    let any_applied = chunk_results
        .iter()
        .any(|c| matches!(c.status, ChunkStatus::Applied));
    if !any_applied {
        return VerifyOutcome::skipped("no applied chunks");
    }

    let Some(config) = config else {
        return VerifyOutcome::skipped("no autofix_verify config");
    };

    if !env_gate_enabled() {
        return VerifyOutcome::skipped("disabled via HOMEBOY_AUTOFIX_VERIFY=0");
    }

    let outcome = run_verify_command(config, root);

    if !outcome.passed {
        log_status!(
            "autofix_verify",
            "Post-apply verify failed ({} in {:?}): reverting {} file(s)",
            describe_exit(&outcome),
            outcome.duration,
            rollback.len()
        );
        rollback.restore_all();
        mark_applied_chunks_reverted(chunk_results, &outcome);
    } else {
        log_status!(
            "autofix_verify",
            "Post-apply verify passed in {:?}",
            outcome.duration
        );
    }

    outcome
}

/// Execute the configured verify command under the configured timeout.
/// Returns `passed=false` on non-zero exit, spawn failure, or timeout.
fn run_verify_command(config: &AutofixVerifyConfig, root: &Path) -> VerifyOutcome {
    let timeout = Duration::from_secs(config.effective_timeout_secs());
    let started = Instant::now();

    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            return VerifyOutcome {
                passed: false,
                exit_code: None,
                combined_output: format!(
                    "failed to spawn verify command '{}': {}",
                    config.command, err
                ),
                duration: started.elapsed(),
                skipped: false,
                reason: "spawn_failed",
            };
        }
    };

    // Poll with a timeout. `wait_timeout` is an external crate; stay
    // dependency-light by polling `try_wait` in a tight loop.
    match wait_with_timeout(child, timeout) {
        WaitResult::Exited(output) => {
            let exit_code = output.status.code();
            let passed = output.status.success();
            VerifyOutcome {
                passed,
                exit_code,
                combined_output: truncate_combined_output(&output.stdout, &output.stderr),
                duration: started.elapsed(),
                skipped: false,
                reason: if passed { "passed" } else { "non_zero_exit" },
            }
        }
        WaitResult::TimedOut => VerifyOutcome {
            passed: false,
            exit_code: None,
            combined_output: format!(
                "verify command '{}' exceeded timeout of {}s",
                config.command,
                config.effective_timeout_secs()
            ),
            duration: started.elapsed(),
            skipped: false,
            reason: "timeout",
        },
        WaitResult::WaitError(msg) => VerifyOutcome {
            passed: false,
            exit_code: None,
            combined_output: format!("wait on verify command failed: {}", msg),
            duration: started.elapsed(),
            skipped: false,
            reason: "wait_failed",
        },
    }
}

enum WaitResult {
    Exited(std::process::Output),
    TimedOut,
    WaitError(String),
}

/// Poll-based wait with a kill-on-timeout fallback. Good enough for verify
/// commands in the 100ms–2min range; avoids pulling in `wait_timeout` /
/// `tokio`. Sleeps 50ms between polls.
fn wait_with_timeout(mut child: std::process::Child, timeout: Duration) -> WaitResult {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return match child.wait_with_output() {
                    Ok(out) => WaitResult::Exited(out),
                    Err(e) => WaitResult::WaitError(e.to_string()),
                };
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return WaitResult::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return WaitResult::WaitError(e.to_string()),
        }
    }
}

/// Combine stdout + stderr into a single display string, truncated to keep
/// downstream JSON envelopes small. Trailing whitespace trimmed.
fn truncate_combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    const LIMIT: usize = 4096;
    let mut out = String::new();
    out.push_str(&String::from_utf8_lossy(stdout));
    if !stderr.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&String::from_utf8_lossy(stderr));
    }
    let trimmed = out.trim_end();
    if trimmed.len() <= LIMIT {
        trimmed.to_string()
    } else {
        let cut = &trimmed[trimmed.len() - LIMIT..];
        format!("[…truncated…]\n{}", cut)
    }
}

fn describe_exit(outcome: &VerifyOutcome) -> String {
    match outcome.exit_code {
        Some(code) => format!("exit {}", code),
        None => match outcome.reason {
            "timeout" => "timeout".to_string(),
            "spawn_failed" => "spawn failed".to_string(),
            "wait_failed" => "wait failed".to_string(),
            _ => "no exit code".to_string(),
        },
    }
}

/// Rewrite every currently-`Applied` chunk to `Reverted`, carrying the verify
/// output into the `error` field so the operator can see why.
fn mark_applied_chunks_reverted(chunks: &mut [ApplyChunkResult], outcome: &VerifyOutcome) {
    let reason = format!(
        "autofix_verify failed ({}): {}",
        describe_exit(outcome),
        outcome.combined_output
    );
    for chunk in chunks.iter_mut() {
        if matches!(chunk.status, ChunkStatus::Applied) {
            chunk.status = ChunkStatus::Reverted;
            chunk.reverted_files = chunk.applied_files;
            chunk.applied_files = 0;
            chunk.verification = Some("autofix_verify_failed".to_string());
            chunk.error = Some(reason.clone());
        }
    }
}

/// Collect the unique list of applied, modified file paths (relative to
/// `root`) from a set of chunk results. Used by callers that need the
/// snapshot scope to line up with what actually changed on disk.
pub fn applied_files_from_chunks(chunks: &[ApplyChunkResult]) -> Vec<PathBuf> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for chunk in chunks.iter() {
        if matches!(chunk.status, ChunkStatus::Applied) {
            for f in &chunk.files {
                seen.insert(f.clone());
            }
        }
    }
    seen.into_iter().map(PathBuf::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refactor::auto::{ApplyChunkResult, ChunkStatus};
    use std::fs;

    fn chunk(name: &str, files: &[&str], status: ChunkStatus) -> ApplyChunkResult {
        let is_applied = matches!(status, ChunkStatus::Applied);
        ApplyChunkResult {
            chunk_id: name.to_string(),
            files: files.iter().map(|s| s.to_string()).collect(),
            status,
            applied_files: if is_applied { files.len() } else { 0 },
            reverted_files: 0,
            verification: Some("write_ok".to_string()),
            error: None,
        }
    }

    #[test]
    fn skip_when_no_applied_chunks() {
        let root = tempfile::tempdir().unwrap();
        let cfg = AutofixVerifyConfig {
            command: "true".to_string(),
            args: vec![],
            timeout_secs: None,
        };
        let rollback = InMemoryRollback::new();
        let mut chunks = vec![chunk("c1", &["a.rs"], ChunkStatus::Reverted)];

        let outcome = run_verify_gate(Some(&cfg), &rollback, root.path(), &mut chunks);

        assert!(outcome.passed);
        assert!(outcome.skipped);
        assert_eq!(outcome.reason, "no applied chunks");
    }

    #[test]
    fn skip_when_no_config() {
        let root = tempfile::tempdir().unwrap();
        let rollback = InMemoryRollback::new();
        let mut chunks = vec![chunk("c1", &["a.rs"], ChunkStatus::Applied)];

        let outcome = run_verify_gate(None, &rollback, root.path(), &mut chunks);

        assert!(outcome.passed);
        assert!(outcome.skipped);
        // Chunks must NOT be downgraded when skipping.
        assert!(matches!(chunks[0].status, ChunkStatus::Applied));
    }

    #[test]
    fn skip_when_env_disabled() {
        let root = tempfile::tempdir().unwrap();
        let cfg = AutofixVerifyConfig {
            command: "true".to_string(),
            args: vec![],
            timeout_secs: None,
        };
        let rollback = InMemoryRollback::new();
        let mut chunks = vec![chunk("c1", &["a.rs"], ChunkStatus::Applied)];

        // SAFETY: tests in this module run serially because they share the
        // same env var. If we ever parallelize, use a per-test mutex.
        std::env::set_var(VERIFY_ENV_VAR, "0");
        let outcome = run_verify_gate(Some(&cfg), &rollback, root.path(), &mut chunks);
        std::env::remove_var(VERIFY_ENV_VAR);

        assert!(outcome.passed);
        assert!(outcome.skipped);
        assert!(outcome.reason.contains("HOMEBOY_AUTOFIX_VERIFY=0"));
        assert!(matches!(chunks[0].status, ChunkStatus::Applied));
    }

    #[test]
    fn passing_verify_leaves_files_and_chunks_alone() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.rs");
        fs::write(&file, "// modified\n").unwrap();

        let cfg = AutofixVerifyConfig {
            command: "true".to_string(),
            args: vec![],
            timeout_secs: None,
        };

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file);
        let mut chunks = vec![chunk("c1", &["a.rs"], ChunkStatus::Applied)];

        let outcome = run_verify_gate(Some(&cfg), &rollback, root.path(), &mut chunks);

        assert!(outcome.passed);
        assert!(!outcome.skipped);
        assert_eq!(outcome.exit_code, Some(0));
        assert!(matches!(chunks[0].status, ChunkStatus::Applied));
        assert_eq!(fs::read_to_string(&file).unwrap(), "// modified\n");
    }

    #[test]
    fn failing_verify_reverts_files_and_downgrades_chunks() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.rs");
        fs::write(&file, "original\n").unwrap();

        // Capture pre-apply state, then "apply" a modification.
        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file);
        fs::write(&file, "modified\n").unwrap();

        let cfg = AutofixVerifyConfig {
            command: "false".to_string(), // always exits 1
            args: vec![],
            timeout_secs: None,
        };
        let mut chunks = vec![chunk("c1", &["a.rs"], ChunkStatus::Applied)];

        let outcome = run_verify_gate(Some(&cfg), &rollback, root.path(), &mut chunks);

        assert!(!outcome.passed);
        assert!(!outcome.skipped);
        assert_eq!(outcome.exit_code, Some(1));

        // File should be back to "original".
        assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");

        // Chunk should be Reverted with autofix_verify_failed verification.
        assert!(matches!(chunks[0].status, ChunkStatus::Reverted));
        assert_eq!(
            chunks[0].verification.as_deref(),
            Some("autofix_verify_failed")
        );
        assert!(chunks[0].error.as_ref().unwrap().contains("exit 1"));
        assert_eq!(chunks[0].applied_files, 0);
        assert_eq!(chunks[0].reverted_files, 1);
    }

    #[test]
    fn failing_verify_removes_newly_created_files() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("new.rs");

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file); // didn't exist yet
        fs::write(&file, "fn main() {}\n").unwrap();

        let cfg = AutofixVerifyConfig {
            command: "false".to_string(),
            args: vec![],
            timeout_secs: None,
        };
        let mut chunks = vec![chunk("nf", &["new.rs"], ChunkStatus::Applied)];

        let outcome = run_verify_gate(Some(&cfg), &rollback, root.path(), &mut chunks);

        assert!(!outcome.passed);
        assert!(
            !file.exists(),
            "newly-created file must be removed on verify failure"
        );
        assert!(matches!(chunks[0].status, ChunkStatus::Reverted));
    }

    #[test]
    fn unknown_command_counts_as_failure_and_reverts() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.rs");
        fs::write(&file, "original\n").unwrap();

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file);
        fs::write(&file, "modified\n").unwrap();

        let cfg = AutofixVerifyConfig {
            command: "this-binary-definitely-does-not-exist-12345".to_string(),
            args: vec![],
            timeout_secs: None,
        };
        let mut chunks = vec![chunk("c1", &["a.rs"], ChunkStatus::Applied)];

        let outcome = run_verify_gate(Some(&cfg), &rollback, root.path(), &mut chunks);

        assert!(!outcome.passed);
        assert_eq!(outcome.reason, "spawn_failed");
        // File reverts so we don't ship broken code just because verify is
        // mis-configured — fail closed.
        assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");
        assert!(matches!(chunks[0].status, ChunkStatus::Reverted));
    }

    #[test]
    fn timeout_triggers_revert() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.rs");
        fs::write(&file, "original\n").unwrap();

        let mut rollback = InMemoryRollback::new();
        rollback.capture(&file);
        fs::write(&file, "modified\n").unwrap();

        let cfg = AutofixVerifyConfig {
            command: "sleep".to_string(),
            args: vec!["5".to_string()],
            timeout_secs: Some(1),
        };
        let mut chunks = vec![chunk("c1", &["a.rs"], ChunkStatus::Applied)];

        let outcome = run_verify_gate(Some(&cfg), &rollback, root.path(), &mut chunks);

        assert!(!outcome.passed);
        assert_eq!(outcome.reason, "timeout");
        assert_eq!(fs::read_to_string(&file).unwrap(), "original\n");
        assert!(matches!(chunks[0].status, ChunkStatus::Reverted));
    }

    #[test]
    fn applied_files_from_chunks_dedups_and_skips_reverted() {
        let chunks = vec![
            chunk("c1", &["a.rs", "b.rs"], ChunkStatus::Applied),
            chunk("c2", &["b.rs", "c.rs"], ChunkStatus::Applied),
            chunk("c3", &["d.rs"], ChunkStatus::Reverted),
        ];

        let files = applied_files_from_chunks(&chunks);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        assert_eq!(names, vec!["a.rs", "b.rs", "c.rs"]);
    }

    #[test]
    fn capture_pre_apply_snapshot_records_all_paths() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.rs");
        let b = root.path().join("b.rs");
        fs::write(&a, "a\n").unwrap();
        fs::write(&b, "b\n").unwrap();

        let rollback = capture_pre_apply_snapshot(root.path(), ["a.rs", "b.rs"]);
        assert_eq!(rollback.len(), 2);
    }
}
