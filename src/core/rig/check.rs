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
use super::spec::{CheckSpec, RigSpec};
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

    if set == 0 {
        return Err(Error::validation_invalid_argument(
            "check",
            "Check must specify one of `http`, `file`, or `command`",
            None,
            None,
        ));
    }
    if set > 1 {
        return Err(Error::validation_invalid_argument(
            "check",
            "Check must specify exactly one of `http`, `file`, or `command`",
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
    Ok(())
}

fn http_check(rig: &RigSpec, url: &str, expect_status: u16) -> Result<()> {
    let resolved = expand_vars(rig, url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| Error::internal_unexpected(format!("build http client: {}", e)))?;

    let response = client.get(&resolved).send().map_err(|e| {
        Error::validation_invalid_argument(
            "check.http",
            format!("HTTP GET {} failed: {}", resolved, e),
            None,
            None,
        )
    })?;

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
    Ok(())
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
