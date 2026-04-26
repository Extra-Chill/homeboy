//! Declarative check evaluation.
//!
//! A `CheckSpec` has optional `http` / `file` / `command` fields. Exactly one
//! should be set per spec. `evaluate` returns `Ok(())` on pass, a structured
//! `Error` on fail.
//!
//! Kept deliberately small — no retries, no fancy wait-for semantics. A
//! failing check means fix-the-env, not poll-until-it-works.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use super::expand::expand_vars;
use super::service::discover_newest;
use super::spec::{CheckSpec, NewerThanSpec, RigSpec, TimeSource};
use crate::error::{Error, Result};

/// Run a check against the current rig state. Err on fail.
pub fn evaluate(rig: &RigSpec, check: &CheckSpec) -> Result<()> {
    let mut set = 0;
    if check.http.is_some() {
        set += 1;
    }
    if check.file.is_some() {
        set += 1;
    }
    if check.command.is_some() {
        set += 1;
    }
    if check.newer_than.is_some() {
        set += 1;
    }

    if set == 0 {
        return Err(Error::validation_invalid_argument(
            "check",
            "Check must specify one of `http`, `file`, `command`, or `newer_than`",
            None,
            None,
        ));
    }
    if set > 1 {
        return Err(Error::validation_invalid_argument(
            "check",
            "Check must specify exactly one of `http`, `file`, `command`, or `newer_than`",
            None,
            None,
        ));
    }

    if let Some(url) = &check.http {
        return http_check(rig, url, check.expect_status.unwrap_or(200));
    }
    if let Some(path) = &check.file {
        return file_check(rig, path, check.contains.as_deref());
    }
    if let Some(cmd) = &check.command {
        return command_check(rig, cmd, check.expect_exit.unwrap_or(0));
    }
    if let Some(spec) = &check.newer_than {
        return newer_than_check(rig, spec);
    }
    Ok(())
}

/// Total wait budget for an HTTP probe to converge on a TCP listener.
///
/// Closes Extra-Chill/homeboy#1537: `service.start` returns once the child
/// is forked, but the kernel may not have called `bind()`/`listen()` yet
/// when the next pipeline step (`service.health`) fires. Connect-refused
/// is a clear "not ready yet" signal — we retry the request, bounded by
/// this ceiling, before giving up. Any HTTP-level response (even 5xx)
/// counts as "the listener is up" and short-circuits the loop, because
/// the question this probe answers is "is the port serving?", not "is
/// the application happy?". Application-level health belongs in a
/// separate `command` check.
const HTTP_WAIT_READY_BUDGET: Duration = Duration::from_secs(10);

/// Per-attempt sleep between connect-refused retries. Short enough that
/// a service that comes up in <100ms still feels instant; long enough
/// that we don't spin the CPU against a slow-starting daemon.
const HTTP_RETRY_INTERVAL: Duration = Duration::from_millis(200);

fn http_check(rig: &RigSpec, url: &str, expect_status: u16) -> Result<()> {
    let resolved = expand_vars(rig, url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::internal_unexpected(format!("build http client: {}", e)))?;

    let deadline = std::time::Instant::now() + HTTP_WAIT_READY_BUDGET;

    loop {
        match client.get(&resolved).send() {
            Ok(response) => {
                let actual = response.status().as_u16();
                if actual != expect_status {
                    return Err(Error::validation_invalid_argument(
                        "check.http",
                        format!(
                            "HTTP GET {} returned {} (expected {})",
                            resolved, actual, expect_status
                        ),
                        None,
                        None,
                    ));
                }
                return Ok(());
            }
            Err(e) if e.is_connect() && std::time::Instant::now() < deadline => {
                // Listener not up yet — keep waiting until the budget runs out.
                // We deliberately do NOT retry on DNS, TLS, or read-timeout
                // errors: those aren't startup races, they're real problems
                // a retry loop would just paper over.
                std::thread::sleep(HTTP_RETRY_INTERVAL);
            }
            Err(e) => {
                // Either a non-connect error (DNS, TLS, read-timeout —
                // surface verbatim) or the wait-ready budget exhausted on
                // a still-refused connection. Either way the latest error
                // is the most accurate diagnostic.
                return Err(Error::validation_invalid_argument(
                    "check.http",
                    format!("HTTP GET {} failed: {}", resolved, e),
                    None,
                    None,
                ));
            }
        }
    }
}

fn file_check(rig: &RigSpec, path: &str, contains: Option<&str>) -> Result<()> {
    let resolved = expand_vars(rig, path);
    let p = PathBuf::from(&resolved);
    if !p.exists() {
        return Err(Error::validation_invalid_argument(
            "check.file",
            format!("File does not exist: {}", resolved),
            None,
            None,
        ));
    }

    if let Some(needle) = contains {
        let content = std::fs::read_to_string(&p).map_err(|e| {
            Error::validation_invalid_argument(
                "check.file",
                format!("Read {} failed: {}", resolved, e),
                None,
                None,
            )
        })?;
        if !content.contains(needle) {
            return Err(Error::validation_invalid_argument(
                "check.file",
                format!(
                    "File {} does not contain expected substring {:?}",
                    resolved, needle
                ),
                None,
                None,
            ));
        }
    }
    Ok(())
}

fn command_check(rig: &RigSpec, cmd: &str, expect_exit: i32) -> Result<()> {
    let resolved = expand_vars(rig, cmd);
    let output = Command::new("sh")
        .arg("-c")
        .arg(&resolved)
        .output()
        .map_err(|e| {
            Error::validation_invalid_argument(
                "check.command",
                format!("Command spawn failed: {}", e),
                None,
                None,
            )
        })?;

    let actual = output.status.code().unwrap_or(-1);
    if actual != expect_exit {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::validation_invalid_argument(
            "check.command",
            format!(
                "Command `{}` exited {} (expected {}){}",
                resolved,
                actual,
                expect_exit,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            ),
            None,
            None,
        ));
    }
    Ok(())
}

/// Mtime comparison: pass when `left > right` (left is newer).
///
/// Asymmetric "missing source" semantics:
/// - `left = process_start` with no matching process ⇒ pass. Interpretation:
///   no stale daemon to flag. The wiki preflight treats "no daemon" and
///   "daemon newer than bundle" as the same ✅ state; this honors that.
/// - Any other missing source (file doesn't exist, right-side process
///   missing) ⇒ fail. The right side is what the left is being compared
///   *against* — its absence breaks the question itself.
fn newer_than_check(rig: &RigSpec, spec: &NewerThanSpec) -> Result<()> {
    let left = resolve_time_source(rig, &spec.left, "left")?;
    let right = resolve_time_source(rig, &spec.right, "right")?;

    match (left, right) {
        // Left process not running — nothing stale to flag.
        (None, _) => Ok(()),
        (Some(_), None) => Err(Error::validation_invalid_argument(
            "check.newer_than.right",
            "Right-side time source is missing — cannot compare against absent reference",
            None,
            None,
        )),
        (Some(l), Some(r)) => {
            if l > r {
                Ok(())
            } else {
                Err(Error::validation_invalid_argument(
                    "check.newer_than",
                    format!(
                        "Left ({}) is not newer than right ({}); diff = {}s",
                        l,
                        r,
                        r as i64 - l as i64
                    ),
                    None,
                    None,
                ))
            }
        }
    }
}

/// Resolve a `TimeSource` to seconds since epoch, or `None` if the source
/// is intentionally absent (only meaningful for left-side `process_start`).
fn resolve_time_source(rig: &RigSpec, src: &TimeSource, side: &str) -> Result<Option<u64>> {
    let mut set = 0;
    if src.file_mtime.is_some() {
        set += 1;
    }
    if src.process_start.is_some() {
        set += 1;
    }
    if set == 0 {
        return Err(Error::validation_invalid_argument(
            format!("check.newer_than.{}", side),
            "Time source must specify one of `file_mtime` or `process_start`",
            None,
            None,
        ));
    }
    if set > 1 {
        return Err(Error::validation_invalid_argument(
            format!("check.newer_than.{}", side),
            "Time source must specify exactly one of `file_mtime` or `process_start`",
            None,
            None,
        ));
    }

    if let Some(path) = &src.file_mtime {
        let resolved = expand_vars(rig, path);
        let meta = std::fs::metadata(&resolved).map_err(|e| {
            Error::validation_invalid_argument(
                format!("check.newer_than.{}.file_mtime", side),
                format!("Stat {} failed: {}", resolved, e),
                None,
                None,
            )
        })?;
        let mtime = meta
            .modified()
            .map_err(|e| {
                Error::validation_invalid_argument(
                    format!("check.newer_than.{}.file_mtime", side),
                    format!("Read mtime of {} failed: {}", resolved, e),
                    None,
                    None,
                )
            })?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| {
                Error::validation_invalid_argument(
                    format!("check.newer_than.{}.file_mtime", side),
                    format!("Bad mtime on {}: {}", resolved, e),
                    None,
                    None,
                )
            })?
            .as_secs();
        return Ok(Some(mtime));
    }

    if let Some(disc) = &src.process_start {
        let pattern = expand_vars(rig, &disc.pattern);
        let proc = discover_newest(&pattern)?;
        return Ok(proc.map(|p| p.started_at_epoch));
    }

    Ok(None)
}

#[cfg(test)]
#[path = "../../../tests/core/rig/check_test.rs"]
mod check_test;
