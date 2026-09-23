//! General hook/event system for lifecycle extensibility.
//!
//! Hooks are shell commands that run at named lifecycle events. Extensions,
//! deploy targets (projects), and components can all declare hooks for a
//! component-scoped event. Extension hooks run first (platform behavior),
//! then deploy-target hooks (site policy), then component hooks (user
//! customization) — see [`resolve_hooks_with_extensions_and_project`].
//!
//! A deploy-target-scoped event (`HookEvent::PostDeployProject`) is a
//! different shape: it is not tied to any one component, so it is never
//! merged from extensions or components — only from the deploy target's own
//! hook map, via [`run_project_scoped_hooks_remote`]. This is deliberate:
//! a step like a site-wide cache purge is a property of *where* you deploy
//! to, not of a component or a platform, and letting extensions declare it
//! would reopen exactly the vendor-name-in-a-generic-layer problem this event
//! exists to avoid (homeboy#14973).
//!
//! Event naming convention: `pre:operation` / `post:operation`
//! Examples: `pre:version:bump`, `post:version:bump`, `post:release`,
//! `post:deploy`, `post:deploy:project`

use crate::component::Component;
use crate::engine::template;
use crate::error::{Error, Result};
use crate::server::{execute_local_command_in_dir, SshClient};
use homeboy_extension_contract::{ExtensionManifest, HookEvent};
use serde::Serialize;
use std::collections::HashMap;

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
/// 1. Extension hooks (platform behavior) — from all linked extensions, sorted by extension ID
/// 2. Component hooks (user customization)
///
/// A configured extension is part of the component's declared behavior. Failing
/// to load it must fail hook resolution rather than silently changing the plan.
///
/// This does not merge in deploy-target (project) hooks. Use
/// [`resolve_hooks_with_project`] where a deploy target is available.
pub fn resolve_hooks(component: &Component, event: HookEvent) -> Result<Vec<String>> {
    resolve_hooks_with_project(component, None, event)
}

/// Resolve hooks for a component, also merging in deploy-target-scoped hooks
/// declared on the project (or another deploy-target-scoped hook map).
///
/// Execution order:
/// 1. Extension hooks (platform behavior)
/// 2. Deploy-target hooks (site policy) — `project_hooks`, when present
/// 3. Component hooks (user customization)
pub fn resolve_hooks_with_project(
    component: &Component,
    project_hooks: Option<&HashMap<HookEvent, Vec<String>>>,
    event: HookEvent,
) -> Result<Vec<String>> {
    let mut manifests = Vec::new();
    if let Some(ref extensions) = component.extensions {
        let mut extension_ids: Vec<_> = extensions.keys().collect();
        extension_ids.sort();
        for extension_id in extension_ids {
            manifests.push(crate::extension::catalog::load_extension(extension_id)?);
        }
    }
    resolve_hooks_with_extensions_and_project(component, &manifests, project_hooks, event)
}

/// Resolve hooks from manifests the caller has already loaded and validated.
///
/// Does not merge in deploy-target (project) hooks. Use
/// [`resolve_hooks_with_extensions_and_project`] where a deploy target is
/// available.
pub fn resolve_hooks_with_extensions(
    component: &Component,
    extensions: &[ExtensionManifest],
    event: HookEvent,
) -> Result<Vec<String>> {
    resolve_hooks_with_extensions_and_project(component, extensions, None, event)
}

/// Resolve hooks from manifests the caller has already loaded and validated,
/// merging in deploy-target-scoped hooks declared on the project (or another
/// deploy-target-scoped hook map) between extension and component hooks.
///
/// Execution order:
/// 1. Extension hooks (platform behavior)
/// 2. Deploy-target hooks (site policy) — `project_hooks`, when present
/// 3. Component hooks (user customization)
pub fn resolve_hooks_with_extensions_and_project(
    component: &Component,
    extensions: &[ExtensionManifest],
    project_hooks: Option<&HashMap<HookEvent, Vec<String>>>,
    event: HookEvent,
) -> Result<Vec<String>> {
    let mut commands = Vec::new();

    if let Some(configured) = component.extensions.as_ref() {
        let mut extension_ids: Vec<_> = configured.keys().collect();
        extension_ids.sort();
        for extension_id in extension_ids {
            let manifest = extensions
                .iter()
                .find(|extension| extension.id == *extension_id)
                .ok_or_else(|| {
                    Error::extension_not_found(
                        extension_id.to_string(),
                        extensions
                            .iter()
                            .map(|extension| extension.id.clone())
                            .collect(),
                    )
                })?;
            if let Some(extension_commands) = manifest.hooks.get(&event) {
                commands.extend(extension_commands.clone());
            }
        }
    }

    // Deploy-target (project) hooks second — site policy, after platform
    // behavior and before the component's own customization.
    if let Some(project_hooks) = project_hooks {
        if let Some(project_commands) = project_hooks.get(&event) {
            commands.extend(project_commands.clone());
        }
    }

    // Component hooks third.
    if let Some(component_commands) = component.hooks.get(&event) {
        commands.extend(component_commands.clone());
    }

    Ok(commands)
}

/// Run all hooks for a given event.
///
/// Resolves hooks from extensions and the component, then executes each command
/// sequentially in the component's `local_path`.
pub fn run_hooks(
    component: &Component,
    event: HookEvent,
    failure_mode: HookFailureMode,
) -> Result<HookRunResult> {
    let commands = resolve_hooks(component, event)?;
    run_commands(&commands, &component.local_path, event, failure_mode)
}

/// Run a list of commands as hooks for a given event.
///
/// This is the low-level executor. Use `run_hooks` for the full resolve+execute flow.
pub fn run_commands(
    commands: &[String],
    working_dir: &str,
    event: HookEvent,
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
        event: event.label().to_string(),
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
///
/// Does not merge in deploy-target (project) hooks. Use
/// [`run_hooks_remote_with_project`] where a deploy target is available.
pub fn run_hooks_remote(
    ssh_client: &SshClient,
    component: &Component,
    event: HookEvent,
    failure_mode: HookFailureMode,
    vars: &HashMap<String, String>,
) -> Result<HookRunResult> {
    run_hooks_remote_with_project(ssh_client, component, None, event, failure_mode, vars)
}

/// Run all hooks for a given event remotely via SSH, also merging in
/// deploy-target-scoped hooks declared on the project (or another
/// deploy-target-scoped hook map).
///
/// Resolution order: extension hooks, then deploy-target hooks, then
/// component hooks — see [`resolve_hooks_with_extensions_and_project`].
pub fn run_hooks_remote_with_project(
    ssh_client: &SshClient,
    component: &Component,
    project_hooks: Option<&HashMap<HookEvent, Vec<String>>>,
    event: HookEvent,
    failure_mode: HookFailureMode,
    vars: &HashMap<String, String>,
) -> Result<HookRunResult> {
    let commands = resolve_hooks_with_project(component, project_hooks, event)?;
    let expanded: Vec<String> = commands
        .iter()
        .map(|c| template::render_map(c, vars))
        .collect();
    run_commands_remote(ssh_client, &expanded, event, failure_mode)
}

/// Run a deploy-target-scoped event (e.g. `HookEvent::PostDeployProject`)
/// remotely via SSH.
///
/// Unlike `run_hooks_remote`/`run_hooks_remote_with_project`, this does not
/// resolve or merge component or extension hooks — a deploy-target-scoped
/// event is not tied to any one component, and (by design) extensions cannot
/// declare it. Only `target_hooks` (the project's own `hooks` map) is
/// consulted.
pub fn run_project_scoped_hooks_remote(
    ssh_client: &SshClient,
    target_hooks: &HashMap<HookEvent, Vec<String>>,
    event: HookEvent,
    failure_mode: HookFailureMode,
    vars: &HashMap<String, String>,
) -> Result<HookRunResult> {
    let commands = target_hooks.get(&event).cloned().unwrap_or_default();
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
pub(crate) fn run_commands_remote(
    ssh_client: &SshClient,
    commands: &[String],
    event: HookEvent,
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
        event: event.label().to_string(),
        commands: results,
        all_succeeded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ScopedExtensionConfig;
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;

    fn write_extension_with_hook(home: &std::path::Path, id: &str, command: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        std::fs::create_dir_all(&extension_dir).unwrap();
        std::fs::write(
            extension_dir.join(format!("{id}.json")),
            format!(
                r#"{{"name":"{id}","version":"1.0.0","hooks":{{"{}": ["{command}"]}}}}"#,
                HookEvent::PreVersionBump
            ),
        )
        .unwrap();
    }

    #[test]
    fn resolve_hooks_returns_empty_when_no_hooks() {
        let component = Component::new(
            "test".to_string(),
            "/tmp/test".to_string(),
            "".to_string(),
            None,
        );
        let commands = resolve_hooks(&component, HookEvent::PreVersionBump).unwrap();
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
        component
            .hooks
            .insert(HookEvent::PreVersionBump, vec!["echo hello".to_string()]);
        let commands = resolve_hooks(&component, HookEvent::PreVersionBump).unwrap();
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
        component
            .hooks
            .insert(HookEvent::PostDeploy, vec!["echo deploy".to_string()]);
        let commands = resolve_hooks(&component, HookEvent::PreVersionBump).unwrap();
        assert!(commands.is_empty());
    }

    #[test]
    fn resolve_hooks_with_project_none_matches_resolve_hooks() {
        let mut component = Component::new(
            "test".to_string(),
            "/tmp/test".to_string(),
            "".to_string(),
            None,
        );
        component
            .hooks
            .insert(HookEvent::PostDeploy, vec!["echo component".to_string()]);

        assert_eq!(
            resolve_hooks_with_project(&component, None, HookEvent::PostDeploy).unwrap(),
            resolve_hooks(&component, HookEvent::PostDeploy).unwrap()
        );
    }

    #[test]
    fn resolve_hooks_with_project_merges_project_hooks_between_extension_and_component() {
        with_isolated_home(|home| {
            write_extension_with_hook(home.path(), "wordpress", "echo extension");
            let mut component = Component::new(
                "test".to_string(),
                "/tmp/test".to_string(),
                "".to_string(),
                None,
            );
            component.extensions = Some(HashMap::from([(
                "wordpress".to_string(),
                ScopedExtensionConfig::default(),
            )]));
            component.hooks.insert(
                HookEvent::PreVersionBump,
                vec!["echo component".to_string()],
            );

            let project_hooks =
                HashMap::from([(HookEvent::PreVersionBump, vec!["echo project".to_string()])]);

            let commands = resolve_hooks_with_project(
                &component,
                Some(&project_hooks),
                HookEvent::PreVersionBump,
            )
            .unwrap();

            assert_eq!(
                commands,
                vec![
                    "echo extension".to_string(),
                    "echo project".to_string(),
                    "echo component".to_string(),
                ]
            );
        });
    }

    #[test]
    fn resolve_hooks_with_project_ignores_project_hooks_for_unrelated_event() {
        let component = Component::new(
            "test".to_string(),
            "/tmp/test".to_string(),
            "".to_string(),
            None,
        );
        let project_hooks =
            HashMap::from([(HookEvent::PostDeploy, vec!["wp cache purge".to_string()])]);

        let commands =
            resolve_hooks_with_project(&component, Some(&project_hooks), HookEvent::PreVersionBump)
                .unwrap();
        assert!(commands.is_empty());
    }

    #[test]
    fn resolve_hooks_with_project_omits_project_only_hooks_when_no_project_hooks_declared() {
        // A project with an empty `hooks` map (or without one at all) must
        // resolve identically to `None` — adding the field must not change
        // existing components' behavior.
        let component = Component::new(
            "test".to_string(),
            "/tmp/test".to_string(),
            "".to_string(),
            None,
        );
        let empty_project_hooks: HashMap<HookEvent, Vec<String>> = HashMap::new();

        assert_eq!(
            resolve_hooks_with_project(
                &component,
                Some(&empty_project_hooks),
                HookEvent::PostDeploy
            )
            .unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn resolve_hooks_sorts_extensions_and_rejects_missing_configured_extension() {
        with_isolated_home(|home| {
            write_extension_with_hook(home.path(), "zebra", "echo zebra");
            write_extension_with_hook(home.path(), "alpha", "echo alpha");
            let mut component = Component::new(
                "test".to_string(),
                "/tmp/test".to_string(),
                "".to_string(),
                None,
            );
            component.extensions = Some(HashMap::from([
                ("zebra".to_string(), ScopedExtensionConfig::default()),
                ("alpha".to_string(), ScopedExtensionConfig::default()),
            ]));

            assert_eq!(
                resolve_hooks(&component, HookEvent::PreVersionBump).unwrap(),
                vec!["echo alpha", "echo zebra"]
            );
            let manifests = vec![
                crate::extension::catalog::load_extension("zebra").unwrap(),
                crate::extension::catalog::load_extension("alpha").unwrap(),
            ];
            assert_eq!(
                resolve_hooks_with_extensions(&component, &manifests, HookEvent::PreVersionBump)
                    .unwrap(),
                vec!["echo alpha", "echo zebra"]
            );

            component
                .extensions
                .as_mut()
                .unwrap()
                .insert("missing".to_string(), ScopedExtensionConfig::default());
            assert!(resolve_hooks(&component, HookEvent::PreVersionBump).is_err());
            assert!(resolve_hooks_with_extensions(
                &component,
                &manifests,
                HookEvent::PreVersionBump
            )
            .is_err());
        });
    }

    #[test]
    fn run_commands_succeeds_with_empty_list() {
        let result =
            run_commands(&[], "/tmp", HookEvent::PostRelease, HookFailureMode::Fatal).unwrap();
        assert!(result.all_succeeded);
        assert!(result.commands.is_empty());
        assert_eq!(result.event, "post:release");
    }

    #[test]
    fn run_commands_executes_successfully() {
        let commands = vec!["echo hello".to_string()];
        let result = run_commands(
            &commands,
            "/tmp",
            HookEvent::PostRelease,
            HookFailureMode::Fatal,
        )
        .unwrap();
        assert!(result.all_succeeded);
        assert_eq!(result.commands.len(), 1);
        assert!(result.commands[0].success);
        assert_eq!(result.commands[0].stdout.trim(), "hello");
    }

    #[test]
    fn run_commands_fatal_stops_on_failure() {
        let commands = vec!["exit 1".to_string(), "echo should-not-run".to_string()];
        let result = run_commands(
            &commands,
            "/tmp",
            HookEvent::PostRelease,
            HookFailureMode::Fatal,
        );
        assert!(result.is_err());
    }

    #[test]
    fn run_commands_non_fatal_continues_on_failure() {
        let commands = vec!["exit 1".to_string(), "echo still-runs".to_string()];
        let result = run_commands(
            &commands,
            "/tmp",
            HookEvent::PostRelease,
            HookFailureMode::NonFatal,
        )
        .unwrap();
        assert!(!result.all_succeeded);
        assert_eq!(result.commands.len(), 2);
        assert!(!result.commands[0].success);
        assert!(result.commands[1].success);
    }

    fn local_ssh_client() -> SshClient {
        SshClient {
            host: "localhost".to_string(),
            user: "test".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: HashMap::new(),
        }
    }

    #[test]
    fn run_project_scoped_hooks_remote_runs_only_target_hooks() {
        let target_hooks = HashMap::from([(
            HookEvent::PostDeployProject,
            vec!["echo {{base_path}}".to_string()],
        )]);
        let vars = HashMap::from([("base_path".to_string(), "/var/www/site".to_string())]);

        let result = run_project_scoped_hooks_remote(
            &local_ssh_client(),
            &target_hooks,
            HookEvent::PostDeployProject,
            HookFailureMode::NonFatal,
            &vars,
        )
        .unwrap();

        assert!(result.all_succeeded);
        assert_eq!(result.commands.len(), 1);
        assert_eq!(result.commands[0].command, "echo /var/www/site");
        assert_eq!(result.commands[0].stdout.trim(), "/var/www/site");
    }

    #[test]
    fn run_project_scoped_hooks_remote_is_empty_when_event_not_declared() {
        let target_hooks = HashMap::from([(
            HookEvent::PostDeploy,
            vec!["echo should-not-run".to_string()],
        )]);

        let result = run_project_scoped_hooks_remote(
            &local_ssh_client(),
            &target_hooks,
            HookEvent::PostDeployProject,
            HookFailureMode::NonFatal,
            &HashMap::new(),
        )
        .unwrap();

        assert!(result.all_succeeded);
        assert!(result.commands.is_empty());
    }

    #[test]
    fn run_project_scoped_hooks_remote_non_fatal_continues_on_failure() {
        let target_hooks = HashMap::from([(
            HookEvent::PostDeployProject,
            vec!["exit 1".to_string(), "echo still-runs".to_string()],
        )]);

        let result = run_project_scoped_hooks_remote(
            &local_ssh_client(),
            &target_hooks,
            HookEvent::PostDeployProject,
            HookFailureMode::NonFatal,
            &HashMap::new(),
        )
        .unwrap();

        assert!(!result.all_succeeded);
        assert_eq!(result.commands.len(), 2);
        assert!(!result.commands[0].success);
        assert!(result.commands[1].success);
    }
}
