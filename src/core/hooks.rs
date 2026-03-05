//! General hook/event system for lifecycle extensibility.
//!
//! Hooks are shell commands that run at named lifecycle events. Both components
//! and extensions can declare hooks. Extension hooks run first (platform behavior),
//! then component hooks (user customization).
//!
//! Event naming convention: `pre:operation` / `post:operation`
//! Examples: `pre:version:bump`, `post:version:bump`, `post:release`, `post:deploy`

use crate::component::Component;
use crate::error::{Error, Result};
use crate::extension;
use crate::ssh::{execute_local_command_in_dir, SshClient};
use crate::utils::template;
use serde::Serialize;
use std::collections::HashMap;

/// A map of event names to command lists.
pub type HookMap = HashMap<String, Vec<String>>;

/// Result of running a single hook command.
#[derive(Debug, Clone, Serialize)]
pub struct HookCommandResult {
    pub command: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Result of running all hooks for an event.
#[derive(Debug, Clone, Serialize)]
pub struct HookRunResult {
    pub event: String,
    pub commands: Vec<HookCommandResult>,
    pub all_succeeded: bool,
}

/// Whether hook failures abort the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFailureMode {
    /// Non-zero exit stops remaining hooks and returns an error.
    Fatal,
    /// Failures are recorded but execution continues.
    NonFatal,
}

/// Resolve all hooks for a given event by merging extension-level and component-level hooks.
///
/// Execution order:
/// 1. Extension hooks (platform behavior) — from all linked extensions, in extension iteration order
/// 2. Component hooks (user customization)
///
/// Legacy fields (`pre_version_bump_commands`, etc.) are migrated into the `hooks` map
/// during deserialization, so they are already included here.
pub fn resolve_hooks(component: &Component, event: &str) -> Vec<String> {
    let mut commands = Vec::new();

    // Extension hooks first
    if let Some(ref extensions) = component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                if let Some(extension_commands) = manifest.hooks.get(event) {
                    commands.extend(extension_commands.clone());
                }
            }
        }
    }

    // Component hooks second (from the new `hooks` map).
    // Legacy fields (pre_version_bump_commands, etc.) are already merged into this
    // map during deserialization via RawComponent, so no separate fallback is needed.
    if let Some(component_commands) = component.hooks.get(event) {
        commands.extend(component_commands.clone());
    }

    commands
}

/// Run all hooks for a given event.
///
/// Resolves hooks from extensions and the component, then executes each command
/// sequentially in the component's `local_path`.
pub fn run_hooks(
    component: &Component,
    event: &str,
    failure_mode: HookFailureMode,
) -> Result<HookRunResult> {
    let commands = resolve_hooks(component, event);
    run_commands(&commands, &component.local_path, event, failure_mode)
}

/// Run a list of commands as hooks for a given event.
///
/// This is the low-level executor. Use `run_hooks` for the full resolve+execute flow.
pub fn run_commands(
    commands: &[String],
    working_dir: &str,
    event: &str,
    failure_mode: HookFailureMode,
) -> Result<HookRunResult> {
    let mut results = Vec::new();
    let mut all_succeeded = true;

    for command in commands {
        let output = execute_local_command_in_dir(command, Some(working_dir), None);

        let result = HookCommandResult {
            command: command.clone(),
            success: output.success,
            stdout: output.stdout.clone(),
            stderr: output.stderr.clone(),
            exit_code: output.exit_code,
        };

        if !output.success {
            all_succeeded = false;

            if failure_mode == HookFailureMode::Fatal {
                let error_text = if output.stderr.trim().is_empty() {
                    &output.stdout
                } else {
                    &output.stderr
                };
                results.push(result);
                return Err(Error::internal_unexpected(format!(
                    "Hook '{}' command failed: {}\n{}",
                    event, command, error_text
                )));
            }
        }

        results.push(result);
    }

    Ok(HookRunResult {
        event: event.to_string(),
        commands: results,
        all_succeeded,
    })
}

/// Run all hooks for a given event remotely via SSH.
///
/// Resolves hooks from extensions and the component, expands template variables
/// (using `{{key}}` syntax), then executes each command on the remote server.
/// Uses the same resolution order as `run_hooks` (extension hooks first, then
/// component hooks).
pub fn run_hooks_remote(
    ssh_client: &SshClient,
    component: &Component,
    event: &str,
    failure_mode: HookFailureMode,
    vars: &HashMap<String, String>,
) -> Result<HookRunResult> {
    let commands = resolve_hooks(component, event);
    let expanded: Vec<String> = commands
        .iter()
        .map(|c| template::render_map(c, vars))
        .collect();
    run_commands_remote(ssh_client, &expanded, event, failure_mode)
}

/// Run a list of commands remotely via SSH.
///
/// This is the low-level remote executor. Use `run_hooks_remote` for the full
/// resolve+expand+execute flow.
pub fn run_commands_remote(
    ssh_client: &SshClient,
    commands: &[String],
    event: &str,
    failure_mode: HookFailureMode,
) -> Result<HookRunResult> {
    let mut results = Vec::new();
    let mut all_succeeded = true;

    for command in commands {
        let output = ssh_client.execute(command);

        let result = HookCommandResult {
            command: command.clone(),
            success: output.success,
            stdout: output.stdout.clone(),
            stderr: output.stderr.clone(),
            exit_code: output.exit_code,
        };

        if !output.success {
            all_succeeded = false;

            if failure_mode == HookFailureMode::Fatal {
                let error_text = if output.stderr.trim().is_empty() {
                    &output.stdout
                } else {
                    &output.stderr
                };
                results.push(result);
                return Err(Error::internal_unexpected(format!(
                    "Hook '{}' command failed: {}\n{}",
                    event, command, error_text
                )));
            }
        }

        results.push(result);
    }

    Ok(HookRunResult {
        event: event.to_string(),
        commands: results,
        all_succeeded,
    })
}

/// Standard event names for the lifecycle hooks.
pub mod events {
    /// Runs after version targets are updated, before git commit.
    pub const PRE_VERSION_BUMP: &str = "pre:version:bump";
    /// Runs after pre-bump hooks, before git commit.
    pub const POST_VERSION_BUMP: &str = "post:version:bump";
    /// Runs after the release pipeline completes.
    pub const POST_RELEASE: &str = "post:release";
    /// Runs after deploy completes.
    pub const POST_DEPLOY: &str = "post:deploy";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_hooks_returns_empty_when_no_hooks() {
        let component = Component::new(
            "test".to_string(),
            "/tmp/test".to_string(),
            "".to_string(),
            None,
        );
        let commands = resolve_hooks(&component, events::PRE_VERSION_BUMP);
        assert!(commands.is_empty());
    }

    #[test]
    fn resolve_hooks_returns_component_hooks() {
        let mut component = Component::new(
            "test".to_string(),
            "/tmp/test".to_string(),
            "".to_string(),
            None,
        );
        component.hooks.insert(
            events::PRE_VERSION_BUMP.to_string(),
            vec!["echo hello".to_string()],
        );
        let commands = resolve_hooks(&component, events::PRE_VERSION_BUMP);
        assert_eq!(commands, vec!["echo hello".to_string()]);
    }

    #[test]
    fn resolve_hooks_ignores_unrelated_events() {
        let mut component = Component::new(
            "test".to_string(),
            "/tmp/test".to_string(),
            "".to_string(),
            None,
        );
        component.hooks.insert(
            events::POST_DEPLOY.to_string(),
            vec!["echo deploy".to_string()],
        );
        let commands = resolve_hooks(&component, events::PRE_VERSION_BUMP);
        assert!(commands.is_empty());
    }

    #[test]
    fn run_commands_succeeds_with_empty_list() {
        let result = run_commands(&[], "/tmp", "test:event", HookFailureMode::Fatal).unwrap();
        assert!(result.all_succeeded);
        assert!(result.commands.is_empty());
        assert_eq!(result.event, "test:event");
    }

    #[test]
    fn run_commands_executes_successfully() {
        let commands = vec!["echo hello".to_string()];
        let result = run_commands(&commands, "/tmp", "test:event", HookFailureMode::Fatal).unwrap();
        assert!(result.all_succeeded);
        assert_eq!(result.commands.len(), 1);
        assert!(result.commands[0].success);
        assert_eq!(result.commands[0].stdout.trim(), "hello");
    }

    #[test]
    fn run_commands_fatal_stops_on_failure() {
        let commands = vec!["exit 1".to_string(), "echo should-not-run".to_string()];
        let result = run_commands(&commands, "/tmp", "test:event", HookFailureMode::Fatal);
        assert!(result.is_err());
    }

    #[test]
    fn run_commands_non_fatal_continues_on_failure() {
        let commands = vec!["exit 1".to_string(), "echo still-runs".to_string()];
        let result =
            run_commands(&commands, "/tmp", "test:event", HookFailureMode::NonFatal).unwrap();
        assert!(!result.all_succeeded);
        assert_eq!(result.commands.len(), 2);
        assert!(!result.commands[0].success);
        assert!(result.commands[1].success);
    }
}
