use super::super::helpers::current_version;
use super::super::helpers::version_is_newer;
use super::*;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerExecOptions;
use crate::core::Result;
use regex::Regex;

pub fn runner_homeboy_version(
    runner: &Runner,
    homeboy_path: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Result<Option<String>> {
    let (output, exit_code) = exec(
        &runner.id,
        runner_exec_options(
            runner,
            vec![homeboy_path.to_string(), "--version".to_string()],
        ),
    )?;
    if exit_code != 0 {
        return Ok(None);
    }

    Ok(parse_cli_version_output(&output.stdout)
        .or_else(|| parse_cli_version_output(&output.stderr)))
}

pub fn runner_homeboy_identity(
    runner: &Runner,
    homeboy_path: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Result<Option<String>> {
    let (output, exit_code) = exec(
        &runner.id,
        runner_exec_options(
            runner,
            vec![homeboy_path.to_string(), "--version".to_string()],
        ),
    )?;
    if exit_code != 0 {
        return Ok(None);
    }

    let output = if output.stdout.trim().is_empty() {
        output.stderr.trim()
    } else {
        output.stdout.trim()
    };

    if output.is_empty() {
        Ok(None)
    } else {
        Ok(Some(output.to_string()))
    }
}

pub fn runner_bare_homeboy_version(
    runner: &Runner,
    homeboy_path: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Option<String> {
    if homeboy_path == "homeboy" {
        return None;
    }

    runner_homeboy_version(runner, "homeboy", exec)
        .ok()
        .flatten()
}

/// Returns the local/current homeboy version used for runner drift comparisons.
///
/// In production this is the compiled crate version (`current_version()`). Tests
/// override it via [`with_local_version_override`] so fixtures can pin a
/// deterministic "local" version instead of coupling to the live crate version,
/// which otherwise climbs past hardcoded fixture versions over time and makes
/// the drift check fire spuriously.
pub fn effective_local_version() -> String {
    #[cfg(test)]
    {
        if let Some(version) = super::tests::local_version_override() {
            return version;
        }
    }
    current_version().to_string()
}

pub fn runner_local_version_drift(
    runner_id: &str,
    homeboy_path: &str,
    previous_version: Option<&str>,
    new_version: Option<&str>,
) -> Option<String> {
    let local_version = effective_local_version();
    let local_version = local_version.as_str();
    let runner_version = new_version?;
    if !version_is_newer(local_version, runner_version) {
        return None;
    }

    Some(format!(
        "configured runner executable `{homeboy_path}` reports {runner_version}, but local/current reports {local_version}; runner before was {}; remediate with `{}`",
        previous_version.unwrap_or("unknown"),
        runner_upgrade_recovery_commands(runner_id)[0]
    ))
}

pub fn parse_cli_version_output(output: &str) -> Option<String> {
    let re = Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(output).map(|m| m.as_str().to_string())
}
