use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::component::{self, TargetSpec};
use homeboy::core::git;
use homeboy::core::lab_routing::{
    self, LabDispatchObserver, LabRouteOutcome, LabRoutingRequest, NoopLabDispatchObserver,
};
use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::runners::{self, RunnerExecOptions};
use homeboy::core::Error;
use std::collections::HashMap;

use crate::commands::utils::output::write_output_file;

pub fn route_after_parse(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    if lab_routing::is_lab_offload_subprocess() {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Runs(args)) = (cli.runner.as_deref(), &cli.command) {
        if !is_runs_list_runner_option(normalized_args) {
            return Err(crate::commands::runs::global_runner_error(args, runner_id));
        }

        return Ok(None);
    }

    if is_command_local_runner_option(&cli.command) {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Rig(args)) = (cli.runner.as_deref(), &cli.command) {
        if args.is_runner_source_management_command() {
            let (stdout, stderr, exit_code) =
                run_rig_source_management_on_runner(runner_id, normalized_args, output_file)?;
            if !stderr.is_empty() {
                eprint!("{stderr}");
            }
            print!("{stdout}");
            return Ok(Some(exit_code));
        }
    }

    let lab_command = lab_offload_command(&cli.command)?;

    let trace_runner_id = if matches!(cli.command, Commands::Trace(_)) {
        cli.runner
            .clone()
            .or_else(|| runners::resolve_default_lab_runner().ok().flatten())
    } else {
        None
    };

    let observer = lab_dispatch_observer(cli, normalized_args, trace_runner_id.as_deref());
    let active_run_id = observer.run_id().map(str::to_string);

    let capture_mutation_patch = cli.command.lab_offload_captures_mutation_patch();
    let mutation_flag = cli.command.lab_offload_mutation_flag();

    // For component-targeted write/fix commands (`homeboy lint --fix <component>`,
    // `homeboy refactor --from lint --write <component>`), the component is
    // resolved on the controller to its source checkout and the args are
    // rewritten to `--path <source>`. Without this, the offload syncs and
    // diff-captures the controller's working directory while the remote re-resolves
    // the positional component to the runner's registered checkout and writes
    // fixes there — so the source-tree mutation lands outside the captured
    // workspace and the runner returns no patch to apply (#4315).
    let scoped_args = inject_lab_changed_files(&cli.command, normalized_args)?;
    let normalized_args = scoped_args.as_deref().unwrap_or(normalized_args);

    let rewritten_args =
        lab_route_source_path_args(&cli.command, normalized_args, capture_mutation_patch);
    let routed_args = rewritten_args.as_deref().unwrap_or(normalized_args);
    let job_overrides = lab_job_overrides(cli)?;

    let outcome = lab_routing::dispatch_lab_offload(
        LabRoutingRequest {
            command: lab_command,
            normalized_args: routed_args,
            explicit_runner: cli.runner.as_deref(),
            force_hot: cli.force_hot,
            local_policy: runners::LabLocalExecutionPolicy::from_flags(
                cli.allow_local_hot,
                cli.allow_local_fallback,
                cli.lab_only,
            ),
            allow_dirty_lab_workspace: cli.allow_dirty_lab_workspace,
            capture_patch: capture_mutation_patch,
            mutation_flag,
            timeout: None,
            active_run_id: active_run_id.as_deref(),
            detach_after_handoff: cli.detach_after_handoff,
            output_file_requested: output_file.is_some(),
            local_output_file: output_file,
            job_overrides,
        },
        trace_runner_id.as_deref(),
        observer,
    )?;

    match outcome {
        LabRouteOutcome::RunLocal => {
            if let Some(warning) = agent_task_local_fanout_warning(&cli.command) {
                eprintln!("{warning}");
            }
            Ok(None)
        }
        LabRouteOutcome::Offloaded(output) => {
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }
            if let Some(path) = output_file {
                write_offloaded_stdout(
                    path,
                    output
                        .output_file_content
                        .as_deref()
                        .unwrap_or(&output.stdout),
                )?;
            }
            print!("{}", output.stdout);
            Ok(Some(output.exit_code))
        }
    }
}

fn lab_job_overrides(cli: &Cli) -> homeboy::core::Result<runners::LabJobOverrides> {
    let mut overrides = runners::LabJobOverrides::default();
    let policy = RedactionPolicy::default();

    for raw in &cli.runner_env {
        let (name, value) = parse_lab_env_pair("runner-env", raw)?;
        if policy.is_sensitive_key(&name) || policy.redact_string(&value) != value {
            overrides.secret_env_names.push(name.clone());
        }
        overrides.env.insert(name, value);
    }

    if let Some(raw_json) = cli.lab_env_json.as_deref() {
        let value: serde_json::Value = serde_json::from_str(raw_json).map_err(|err| {
            Error::validation_invalid_argument(
                "lab-env-json",
                format!("--lab-env-json must be a JSON object: {err}"),
                Some(raw_json.to_string()),
                None,
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            Error::validation_invalid_argument(
                "lab-env-json",
                "--lab-env-json must be a JSON object of string or null values",
                Some(raw_json.to_string()),
                None,
            )
        })?;
        for (name, value) in object {
            let name = validate_lab_env_name("lab-env-json", name)?;
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Null => String::new(),
                _ => {
                    return Err(Error::validation_invalid_argument(
                        "lab-env-json",
                        format!("--lab-env-json value for `{name}` must be a string or null"),
                        Some(value.to_string()),
                        None,
                    ));
                }
            };
            if policy.is_sensitive_key(&name) || policy.redact_string(&value) != value {
                overrides.secret_env_names.push(name.clone());
            }
            overrides.env.insert(name, value);
        }
    }

    overrides.secret_env_names.sort();
    overrides.secret_env_names.dedup();
    overrides.workspace_root = cli
        .runner_workspace_root
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(overrides)
}

fn parse_lab_env_pair(source: &str, raw: &str) -> homeboy::core::Result<(String, String)> {
    let (name, value) = raw.split_once('=').ok_or_else(|| {
        Error::validation_invalid_argument(
            source,
            format!("--{source} expects KEY=VALUE"),
            Some(raw.to_string()),
            None,
        )
    })?;
    Ok((validate_lab_env_name(source, name)?, value.to_string()))
}

fn validate_lab_env_name(source: &str, name: &str) -> homeboy::core::Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.contains('=')
        || !name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(Error::validation_invalid_argument(
            source,
            format!("--{source} environment names must be non-empty ASCII identifiers"),
            Some(name.to_string()),
            None,
        ));
    }
    Ok(name.to_string())
}

fn agent_task_local_fanout_warning(command: &Commands) -> Option<String> {
    let (label, concurrency, task_count) = match command {
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::Cook(args),
        }) => (
            "agent-task cook local fanout",
            args.dispatch.concurrency,
            args.dispatch.tasks.len()
                + usize::from(args.dispatch.prompt.is_some())
                + usize::from(args.dispatch.core.tasks_json.is_some()),
        ),
        _ => return None,
    };

    (concurrency > 1 || task_count > 1).then(|| {
        format!(
            "HOMEBOY_LOCAL_FANOUT_WARNING: {label} will execute on this controller with concurrency={concurrency}, tasks={task_count}, execution_location=local. Use --runner <runner-id> or --lab-only to prevent local provider fanout."
        )
    })
}

fn inject_lab_changed_files(
    command: &Commands,
    normalized_args: &[String],
) -> homeboy::core::Result<Option<Vec<String>>> {
    let Some((component_id, path_override, changed_since, changed_only)) =
        changed_scope_request(command)
    else {
        return Ok(None);
    };
    if has_lab_changed_files_json(normalized_args) {
        return Ok(None);
    }
    if changed_since.is_some() || !changed_only {
        return Ok(None);
    }

    let source_path = resolve_changed_scope_source_path(component_id, path_override)?;
    let changed_files = git::get_dirty_files(&source_path)?;
    let payload = serde_json::to_string(&changed_files).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to encode Lab changed-file payload: {error}"
        ))
    })?;

    let mut rewritten = Vec::with_capacity(normalized_args.len() + 2);
    let insert_at = normalized_args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(normalized_args.len());
    rewritten.extend_from_slice(&normalized_args[..insert_at]);
    rewritten.push("--lab-changed-files-json".to_string());
    rewritten.push(payload);
    rewritten.extend_from_slice(&normalized_args[insert_at..]);
    Ok(Some(rewritten))
}

type ChangedScopeRequest<'a> = (
    Option<&'a String>,
    Option<&'a String>,
    Option<&'a str>,
    bool,
);

fn changed_scope_request(command: &Commands) -> Option<ChangedScopeRequest<'_>> {
    match command {
        Commands::Lint(args) => Some((
            args.comp.component.as_ref(),
            args.comp.path.as_ref(),
            args.changed_since.as_deref(),
            args.changed_only,
        )),
        Commands::Review(args) => Some((
            args.comp.component.as_ref(),
            args.comp.path.as_ref(),
            args.changed_since.as_deref(),
            args.changed_only,
        )),
        Commands::Test(args) => Some((
            args.comp.component.as_ref(),
            args.comp.path.as_ref(),
            args.changed_since.as_deref(),
            false,
        )),
        _ => None,
    }
}

fn has_lab_changed_files_json(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--lab-changed-files-json" || arg.starts_with("--lab-changed-files-json=")
    })
}

fn resolve_changed_scope_source_path(
    component_id: Option<&String>,
    path_override: Option<&String>,
) -> homeboy::core::Result<String> {
    let target = component::resolve_target(TargetSpec::new(
        component_id.map(String::as_str),
        path_override.map(String::as_str),
    ))?;
    Ok(target.source_path.to_string_lossy().to_string())
}

/// Build the Lab dispatch observer for the parsed command. Only `trace`
/// participates in dispatch observation; every other command uses the no-op
/// observer. The core routing service owns the observation lifecycle; this
/// adapter only supplies the implementation.
fn lab_dispatch_observer(
    cli: &Cli,
    normalized_args: &[String],
    trace_runner_id: Option<&str>,
) -> Box<dyn LabDispatchObserver> {
    match &cli.command {
        Commands::Trace(args) => crate::commands::trace::start_lab_dispatch_observation(
            args,
            normalized_args,
            trace_runner_id,
        )
        .map(|observation| Box::new(observation) as Box<dyn LabDispatchObserver>)
        .unwrap_or_else(|| Box::new(NoopLabDispatchObserver)),
        _ => Box::new(NoopLabDispatchObserver),
    }
}

fn run_rig_source_management_on_runner(
    runner_id: &str,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<(String, String, i32)> {
    let runner = runners::load(runner_id)?;
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let command = runner_rig_source_management_command(homeboy_path, normalized_args);

    // Remote-execution preflight before dispatching caller-derived argv to the
    // runner (#5093):
    // 1. Path-translation: reject any forwarded argument that still embeds the
    //    controller-local working directory instead of the runner-resident
    //    workspace, so a controller-only path never reaches the remote runtime.
    // 2. Capability parity: validate the runner can run the forwarded `homeboy`
    //    binary before execution starts (enforced by `runners::exec` against the
    //    supplied `RunnerCapabilityPreflight`).
    let remote_cwd = runner.workspace_root.clone().unwrap_or_default();
    if let Ok(local_cwd) = std::env::current_dir() {
        runners::preflight_remote_argv_path_translation(
            "Rig source management",
            runner_id,
            &command,
            &local_cwd,
            &remote_cwd,
        )?;
    }
    let required_commands: Vec<String> = command
        .first()
        .filter(|program| !program.trim().is_empty())
        .cloned()
        .into_iter()
        .collect();
    let capability_preflight =
        (!required_commands.is_empty()).then(|| runners::RunnerCapabilityPreflight {
            command: "rig.source-management".to_string(),
            required_commands,
            ..Default::default()
        });

    let (output, exit_code) = runners::exec(
        runner_id,
        RunnerExecOptions {
            cwd: runner.workspace_root.clone(),
            project_id: None,
            allow_diagnostic_ssh: false,
            command,
            env: HashMap::new(),
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            capability_preflight,
            required_extensions: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
        },
    )?;

    if let Some(path) = output_file {
        write_offloaded_stdout(path, &output.stdout)?;
    }

    Ok((output.stdout, output.stderr, exit_code))
}

fn runner_rig_source_management_command(
    homeboy_path: &str,
    normalized_args: &[String],
) -> Vec<String> {
    let mut command = vec![homeboy_path.to_string()];
    let mut iter = normalized_args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--runner" || arg == "--output" || arg == "--artifact-root" {
            iter.next();
            continue;
        }
        if arg == "--allow-local-fallback"
            || arg == "--allow-dirty-lab-workspace"
            || arg == "--allow-local-hot"
        {
            continue;
        }
        if arg.starts_with("--runner=")
            || arg.starts_with("--output=")
            || arg.starts_with("--artifact-root=")
        {
            continue;
        }
        command.push(arg.clone());
    }
    command
}

fn is_runs_list_runner_option(args: &[String]) -> bool {
    let Some(runs_index) = args.iter().position(|arg| arg == "runs") else {
        return false;
    };
    let Some(list_index) = args.iter().position(|arg| arg == "list") else {
        return false;
    };

    list_index > runs_index
        && args.iter().enumerate().any(|(index, arg)| {
            index > list_index && (arg == "--runner" || arg.starts_with("--runner="))
        })
}

fn is_command_local_runner_option(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::Doctor(_),
        })
    )
}

fn write_offloaded_stdout(path: &str, stdout: &str) -> homeboy::core::Result<()> {
    write_output_file(path, stdout)
}

fn lab_offload_command(
    command: &Commands,
) -> homeboy::core::Result<Option<runners::LabOffloadCommand>> {
    let Some(route_contract) = command.lab_route_contract()? else {
        return Ok(None);
    };
    Ok(Some(lab_routing::lab_offload_command_from_route_contract(
        route_contract,
    )))
}

fn lab_route_source_path_args(
    command: &Commands,
    normalized_args: &[String],
    capture_mutation_patch: bool,
) -> Option<Vec<String>> {
    if capture_mutation_patch {
        if let Some(rewritten) = rewrite_component_target_to_path(command, normalized_args) {
            return Some(rewritten);
        }
    }

    rewrite_ad_hoc_lab_workspace_to_path(command, normalized_args)
}

/// When a `lint --fix` / `refactor --write` command targets a component by id
/// (positionally or via `--component`/`--components`), resolve that component to
/// its on-disk source path and rewrite the offload args to `--path <source>`.
///
/// The Lab offload patch-capture pipeline (`lab_offload_source_path` →
/// workspace sync → before/after diff) keys entirely off the resolved source
/// path. A bare positional component id resolves to the controller working
/// directory for the sync/diff, but on the remote runner it re-resolves to the
/// runner's registered component checkout — so write fixes land outside the
/// captured workspace and no patch is produced (#4315). Rewriting to `--path`
/// makes the synced workspace, the remote command's working tree, and the
/// captured diff all reference the same directory.
///
/// Returns `None` (leaving args untouched) when there is nothing to rewrite:
/// no component target, an explicit `--path` is already present, the component
/// cannot be resolved, or the command is not a component-targeted lint/refactor.
fn rewrite_component_target_to_path(
    command: &Commands,
    normalized_args: &[String],
) -> Option<Vec<String>> {
    let (component_id, has_path_override) = match command {
        Commands::Lint(args) => (
            args.lab_offload_positional_component(),
            args.lab_offload_has_path_override(),
        ),
        Commands::Refactor(args) if args.is_hot_resource_command() => (
            args.lab_offload_positional_component(),
            args.lab_offload_has_path_override(),
        ),
        _ => return None,
    };

    if has_path_override {
        return None;
    }
    let component_id = component_id?;

    let source_path = resolve_component_source_path(&component_id)?;
    Some(strip_component_target_args(
        normalized_args,
        &component_id,
        &source_path,
    ))
}

/// Resolve a component id to its canonical on-disk source path. Returns `None`
/// when resolution fails so the caller can fall back to the original args and
/// let the normal offload path surface any downstream error.
fn resolve_component_source_path(component_id: &str) -> Option<String> {
    let target = component::resolve_target(TargetSpec::new(Some(component_id), None)).ok()?;
    Some(target.source_path.to_string_lossy().to_string())
}

/// Lab sync already materializes the controller CWD when no explicit source path
/// is supplied. For component commands, make that implicit source explicit so
/// the runner re-enters the command through `--path <runner-workspace>` and can
/// synthesize an ad-hoc component instead of requiring registry state there.
fn rewrite_ad_hoc_lab_workspace_to_path(
    command: &Commands,
    normalized_args: &[String],
) -> Option<Vec<String>> {
    let needs_path = match command {
        Commands::Audit(args) => args.comp.component.is_none() && args.comp.path.is_none(),
        Commands::Lint(args) => args.comp.component.is_none() && args.comp.path.is_none(),
        Commands::Test(args) => args.comp.component.is_none() && args.comp.path.is_none(),
        _ => false,
    };
    if !needs_path {
        return None;
    }

    let source_path = std::env::current_dir().ok()?;
    Some(insert_path_arg_before_passthrough(
        normalized_args,
        &source_path.to_string_lossy(),
    ))
}

fn insert_path_arg_before_passthrough(
    normalized_args: &[String],
    source_path: &str,
) -> Vec<String> {
    let mut rewritten = Vec::with_capacity(normalized_args.len() + 2);
    let mut inserted = false;
    for arg in normalized_args {
        if !inserted && arg == "--" {
            rewritten.push("--path".to_string());
            rewritten.push(source_path.to_string());
            inserted = true;
        }
        rewritten.push(arg.clone());
    }
    if !inserted {
        rewritten.push("--path".to_string());
        rewritten.push(source_path.to_string());
    }
    rewritten
}

/// Drop component-targeting args (the bare positional id and any
/// `-c`/`--component`/`--components` flags) and append `--path <source_path>`.
fn strip_component_target_args(
    normalized_args: &[String],
    component_id: &str,
    source_path: &str,
) -> Vec<String> {
    let mut rewritten = Vec::with_capacity(normalized_args.len() + 1);
    let mut iter = normalized_args.iter().peekable();
    let mut passthrough = false;
    let mut positional_stripped = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            rewritten.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            rewritten.push(arg.clone());
            continue;
        }
        // Flagged component selectors that consume a following value.
        if arg == "-c" || arg == "--component" || arg == "--components" {
            let _ = iter.next();
            continue;
        }
        // Inline `--component=<id>` / `--components=<list>` / `-c<id>` forms.
        if arg.starts_with("--component=")
            || arg.starts_with("--components=")
            || (arg.starts_with("-c") && arg.len() > 2 && !arg.starts_with("--"))
        {
            continue;
        }
        // The bare positional component token (strip only the first match so an
        // unrelated later argument that happens to equal the id is preserved).
        if !positional_stripped && !arg.starts_with('-') && arg == component_id {
            positional_stripped = true;
            continue;
        }
        rewritten.push(arg.clone());
    }
    rewritten.push("--path".to_string());
    rewritten.push(source_path.to_string());
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use homeboy::command_contract::lab_runner_supports_contract_label;
    use std::fs;
    use std::path::Path;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::tempdir;

    struct EnvGuard {
        name: &'static str,
        previous: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }

    struct CwdGuard {
        previous: std::path::PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self {
                name,
                previous,
                _guard: guard,
            }
        }

        fn remove(name: &'static str) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::var(name).ok();
            std::env::remove_var(name);
            Self {
                name,
                previous,
                _guard: guard,
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    impl CwdGuard {
        fn set(path: &std::path::Path) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(path).expect("set current dir");
            Self {
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("restore current dir");
        }
    }

    fn write_rig_source_metadata(home: &Path, rig_id: &str, linked: bool) {
        let sources_dir = home.join(".config").join("homeboy").join("rig-sources");
        fs::create_dir_all(&sources_dir).expect("create rig sources dir");
        let metadata = serde_json::json!({
            "source": "/tmp/rig-package",
            "source_root": "/tmp/rig-package",
            "package_path": "/tmp/rig-package",
            "rig_path": format!("/tmp/rig-package/rigs/{rig_id}/rig.json"),
            "discovery_path": "/tmp/rig-package",
            "linked": linked,
            "materialized": false
        });
        fs::write(
            sources_dir.join(format!("{rig_id}.json")),
            serde_json::to_string_pretty(&metadata).expect("serialize rig source metadata"),
        )
        .expect("write rig source metadata");
    }

    #[test]
    fn non_lab_command_continues_local_dispatch() {
        let cli = Cli::parse_from(["homeboy", "status"]);

        let outcome = route_after_parse(&cli, &["homeboy".into(), "status".into()], None).unwrap();

        assert_eq!(outcome, None);
    }

    #[test]
    fn changed_scope_lint_is_lab_portable() {
        let cli = Cli::parse_from(["homeboy", "lint", "--changed-since", "origin/main"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "lint");
        assert!(command.portable);
        assert!(command.unsupported_reason.is_none());
    }

    #[test]
    fn explicit_runner_for_changed_scope_test_is_lab_portable() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "test",
            "--changed-since",
            "origin/main",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert_eq!(command.hot_label, "test");
        assert!(command.portable);
        assert!(command.unsupported_reason.is_none());
    }

    #[test]
    fn lab_job_overrides_parse_env_json_and_workspace_root() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner-env",
            "STUDIO_NATIVE_TRACE_WP_CODEBOX_PLUGIN_PATH=/tmp/wp-codebox",
            "--runner-env",
            "API_TOKEN=secret-token",
            "--lab-env-json",
            r#"{"EXTRA_PATH":"/tmp/extra","EMPTY":null}"#,
            "--runner-workspace-root",
            "/srv/job-workspace",
            "test",
            "studio-native",
        ]);

        let overrides = lab_job_overrides(&cli).expect("overrides");

        assert_eq!(
            overrides.env["STUDIO_NATIVE_TRACE_WP_CODEBOX_PLUGIN_PATH"],
            "/tmp/wp-codebox"
        );
        assert_eq!(overrides.env["EXTRA_PATH"], "/tmp/extra");
        assert_eq!(overrides.env["EMPTY"], "");
        assert_eq!(
            overrides.workspace_root.as_deref(),
            Some("/srv/job-workspace")
        );
        assert!(overrides
            .secret_env_names
            .contains(&"API_TOKEN".to_string()));
    }

    #[test]
    fn lab_job_overrides_reject_invalid_env_shapes() {
        let cli = Cli::parse_from(["homeboy", "--runner-env", "NO_EQUALS", "test"]);
        let err = lab_job_overrides(&cli).expect_err("invalid pair");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");

        let cli = Cli::parse_from(["homeboy", "--lab-env-json", "[]", "test"]);
        let err = lab_job_overrides(&cli).expect_err("invalid json object");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn changed_since_lint_keeps_git_scope_for_lab_runner() {
        let normalized = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--changed-since".to_string(),
            "origin/main".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let rewritten = inject_lab_changed_files(&cli.command, &normalized).unwrap();

        assert!(rewritten.is_none());
    }

    #[test]
    fn changed_since_test_keeps_git_scope_for_lab_runner() {
        let normalized = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "test".to_string(),
            "--changed-since=origin/main".to_string(),
            "--skip-lint".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let rewritten = inject_lab_changed_files(&cli.command, &normalized).unwrap();

        assert!(rewritten.is_none());
    }

    #[test]
    fn lab_offload_subprocess_skips_recursive_lab_routing() {
        let _env = EnvGuard::set(
            homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV,
            r#"{"status":"offloaded"}"#,
        );
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "trace",
            "--rig",
            "gutenberg-pattern-preview-assets",
            "gutenberg",
            "pattern-preview-assets",
        ]);
        let normalized = [
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "trace".to_string(),
            "--rig".to_string(),
            "gutenberg-pattern-preview-assets".to_string(),
            "gutenberg".to_string(),
            "pattern-preview-assets".to_string(),
        ];

        let outcome = route_after_parse(&cli, &normalized, None).unwrap();

        assert_eq!(outcome, None);
    }

    #[test]
    fn agent_task_doctor_runner_option_routes_locally() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "doctor".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--repair".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("agent-task doctor owns --runner and should not be Lab-routed");

        assert_eq!(outcome, None);
        assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
    }

    #[test]
    fn trace_lab_dispatch_timeout_reads_env_override() {
        let _env = EnvGuard::set(lab_routing::LAB_TRACE_DISPATCH_TIMEOUT_ENV, "7");

        assert_eq!(
            lab_routing::lab_trace_dispatch_timeout(),
            std::time::Duration::from_secs(7)
        );
    }

    #[test]
    fn offloaded_stdout_write_preserves_bytes_for_output_file() {
        let dir = tempdir().unwrap();
        let output_path = dir.path().join("out.json");

        write_offloaded_stdout(&output_path.to_string_lossy(), "{\"ok\":true}\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(output_path).unwrap(),
            "{\"ok\":true}\n"
        );
    }

    #[test]
    fn runner_rig_source_management_command_strips_controller_globals() {
        let normalized = vec![
            "homeboy".to_string(),
            "rig".to_string(),
            "sources".to_string(),
            "list".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--output=./sources.json".to_string(),
            "--allow-local-fallback".to_string(),
        ];

        assert_eq!(
            runner_rig_source_management_command("/usr/local/bin/homeboy", &normalized),
            vec![
                "/usr/local/bin/homeboy".to_string(),
                "rig".to_string(),
                "sources".to_string(),
                "list".to_string(),
            ]
        );
    }

    #[test]
    fn linked_local_rig_check_disables_default_lab_offload() {
        let temp_home = tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", temp_home.path().to_str().expect("home path"));
        write_rig_source_metadata(temp_home.path(), "linked-local", true);
        let cli = Cli::parse_from(["homeboy", "rig", "check", "linked-local"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "rig check");
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert!(!command.routing_policy.infer_source_path_tools);
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn linked_local_rig_check_stays_local_without_runner() {
        // Scope the offload-metadata env var so a parallel test that sets it
        // (process-global) cannot leak into this local/no-runner assertion.
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let temp_home = tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", temp_home.path().to_str().expect("home path"));
        write_rig_source_metadata(temp_home.path(), "linked-local", true);
        let normalized = vec![
            "homeboy".to_string(),
            "rig".to_string(),
            "check".to_string(),
            "linked-local".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("linked local rig check should skip automatic Lab offload");

        assert_eq!(outcome, None);
        assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
    }

    #[test]
    fn installed_git_rig_check_keeps_default_lab_offload() {
        let temp_home = tempdir().expect("temp home");
        let _home = EnvGuard::set("HOME", temp_home.path().to_str().expect("home path"));
        write_rig_source_metadata(temp_home.path(), "installed-git", false);
        let cli = Cli::parse_from(["homeboy", "rig", "check", "installed-git"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "rig check");
        assert!(command.portable);
        assert!(command.routing_policy.default_lab_offload);
        assert!(!command.routing_policy.infer_source_path_tools);
    }

    #[test]
    fn lab_command_preserves_portable_contract_shape() {
        let cli = Cli::parse_from(["homeboy", "lint"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "lint");
        assert!(command.portable);
        assert!(command.unsupported_reason.is_none());
        assert!(command.routing_policy.requires_extension_parity);
    }

    #[test]
    fn extension_update_requires_explicit_lab_runner_without_extension_parity() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "lab",
            "extension",
            "update",
            "wordpress",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "extension update");
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert!(command.unsupported_reason.is_none());
        assert!(!command.routing_policy.requires_extension_parity);
        assert!(command.required_extensions.is_empty());
        assert!(!command.routing_policy.infer_source_path_tools);
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn extension_refresh_requires_explicit_lab_runner_without_extension_parity() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "lab",
            "extension",
            "refresh",
            "https://github.com/Extra-Chill/homeboy-extensions.git",
            "--id",
            "wordpress",
            "--ref",
            "6ff93f43",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "extension update");
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert!(command.unsupported_reason.is_none());
        assert!(!command.routing_policy.requires_extension_parity);
        assert!(command.required_extensions.is_empty());
        assert!(!command.routing_policy.infer_source_path_tools);
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn extension_update_routes_locally_without_explicit_lab_runner() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "extension".to_string(),
            "update".to_string(),
            "wordpress".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("extension update without --runner should not offload");

        assert_eq!(outcome, None);
        assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
    }

    #[test]
    fn other_extension_commands_stay_local_only() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "lab",
            "extension",
            "show",
            "wordpress",
        ]);

        assert!(lab_offload_command(&cli.command).unwrap().is_none());
        assert!(!cli.command.supports_lab_runner());
    }

    #[test]
    fn global_runner_for_runs_show_has_local_mirror_guidance() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "runs",
            "show",
            "run-123",
        ]);

        let err = route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "show".into(),
                "run-123".into(),
            ],
            None,
        )
        .expect_err("runs show rejects global runner with guidance");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("homeboy runs show run-123"));
        assert!(err.message.contains("without --runner"));
    }

    #[test]
    fn runs_list_runner_option_after_subcommand_routes_locally() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);

        for normalized in [
            vec![
                "homeboy".to_string(),
                "runs".to_string(),
                "list".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "--status".to_string(),
                "running".to_string(),
                "--limit".to_string(),
                "20".to_string(),
            ],
            vec![
                "homeboy".to_string(),
                "runs".to_string(),
                "list".to_string(),
                "--runner=homeboy-lab".to_string(),
                "--status".to_string(),
                "running".to_string(),
                "--limit".to_string(),
                "20".to_string(),
            ],
        ] {
            let cli = Cli::parse_from(&normalized);

            let outcome = route_after_parse(&cli, &normalized, None)
                .expect("runs list subcommand runner option should not be rejected");

            assert_eq!(outcome, None);
        }
    }

    #[test]
    fn global_runner_for_runs_list_keeps_placement_guidance() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "runs".to_string(),
            "list".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let err = route_after_parse(&cli, &normalized, None)
            .expect_err("top-level runner on runs list should keep placement guidance");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("homeboy runs list --runner homeboy-lab"));
    }

    #[test]
    fn agent_task_inspection_commands_support_runner_resident_recovery() {
        for args in [
            ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);
            let command = lab_offload_command(&cli.command).unwrap().unwrap();
            assert!(lab_runner_supports_contract_label(command.hot_label));
            assert_eq!(
                command.source_path_mode,
                runners::LabOffloadSourcePathMode::RunnerResident
            );
            assert_eq!(
                command.workspace_mode_policy,
                runners::LabOffloadWorkspaceModePolicy::RunnerResident
            );
            assert!(!command.routing_policy.default_lab_offload);
        }
    }

    #[test]
    fn agent_task_retry_run_supports_explicit_runner() {
        for args in [
            [
                "homeboy",
                "--runner",
                "homeboy-lab",
                "agent-task",
                "retry",
                "agent-task-123",
                "--run",
            ],
            [
                "homeboy",
                "agent-task",
                "retry",
                "agent-task-123",
                "--run",
                "--runner",
                "homeboy-lab",
            ],
        ] {
            let cli = Cli::parse_from(args);

            let command = lab_offload_command(&cli.command).unwrap().unwrap();

            assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
            assert!(lab_runner_supports_contract_label(command.hot_label));
            assert!(command.portable);
            assert!(command.routing_policy.default_lab_offload);
        }
    }

    #[test]
    fn agent_task_providers_supports_explicit_runner_discovery() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "providers",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(lab_runner_supports_contract_label(command.hot_label));
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert!(!command.routing_policy.requires_extension_parity);
        assert!(command.required_extensions.is_empty());
        assert!(!command.routing_policy.infer_source_path_tools);
    }

    #[test]
    fn agent_task_controller_run_from_spec_supports_lab_only_runner_routing() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "--lab-only",
            "agent-task",
            "controller",
            "run-from-spec",
            "loop.json",
            "--max-actions",
            "1",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();
        let local_policy = runners::LabLocalExecutionPolicy::from_flags(
            cli.allow_local_hot,
            cli.allow_local_fallback,
            cli.lab_only,
        );

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(local_policy.deny_local_execution());
        assert_eq!(
            command.hot_label,
            "agent-task controller from-spec --resume/run-from-spec/materialize"
        );
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
        );
    }

    #[test]
    fn agent_task_fanout_submit_batch_requires_explicit_runner_under_lab_only() {
        // Isolate from a parallel test leaking the offload-metadata env var,
        // which would otherwise short-circuit route_after_parse as a Lab
        // offload subprocess and return Ok(None) instead of the deny error.
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "--lab-only".to_string(),
            "agent-task".to_string(),
            "fanout".to_string(),
            "submit-batch".to_string(),
            "--input".to_string(),
            "fanout.json".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();
        assert_eq!(command.hot_label, "agent-task fanout submit-batch");
        assert!(!command.routing_policy.default_lab_offload);
        assert!(!command.routing_policy.infer_source_path_tools);

        let err = route_after_parse(&cli, &normalized, None)
            .expect_err("fanout submit-batch must not run locally under --lab-only");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("Lab-only execution refused local execution"));
        assert!(err.message.contains("automatic Lab offload disabled"));
    }

    #[test]
    fn agent_task_fanout_state_reads_are_runner_resident() {
        for args in [
            [
                "homeboy",
                "--runner",
                "homeboy-lab",
                "agent-task",
                "fanout",
                "status",
                "fanout-batch-123",
            ],
            [
                "homeboy",
                "--runner",
                "homeboy-lab",
                "agent-task",
                "fanout",
                "artifacts",
                "fanout-batch-123",
            ],
        ] {
            let cli = Cli::parse_from(args);

            let command = lab_offload_command(&cli.command).unwrap().unwrap();

            assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
            assert_eq!(command.hot_label, "agent-task fanout status/artifacts");
            assert!(command.portable);
            assert!(!command.routing_policy.default_lab_offload);
            assert_eq!(
                command.source_path_mode,
                runners::LabOffloadSourcePathMode::RunnerResident
            );
            assert_eq!(
                command.workspace_mode_policy,
                runners::LabOffloadWorkspaceModePolicy::RunnerResident
            );
            assert!(command.required_extensions.is_empty());
            assert!(!command.routing_policy.requires_extension_parity);
            assert!(!command.routing_policy.infer_source_path_tools);
        }
    }

    #[test]
    fn tunnel_service_start_supports_explicit_runner_discovery() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "tunnel",
            "service",
            "start",
            "preview",
            "--cwd",
            "/home/user/Developer/_lab_workspaces/site",
            "--command",
            "npm run dev",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert_eq!(command.hot_label, "tunnel service start");
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert_eq!(
            command.source_path_mode,
            runners::LabOffloadSourcePathMode::RunnerResident
        );
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::RunnerResident
        );
        assert!(!command.routing_policy.requires_extension_parity);
        assert!(command.required_extensions.is_empty());
        assert!(!command.routing_policy.infer_source_path_tools);
    }

    #[test]
    fn tunnel_preview_consumer_run_keeps_explicit_runner_contract() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "tunnel",
            "preview-consumer",
            "run",
            "--config",
            "consumer.json",
            "--preview-public-url",
            "https://preview.example.test/run",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "tunnel preview-consumer run");
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
    }

    #[test]
    fn lab_command_with_mutation_flag_stays_portable_for_patch_capture() {
        let cli = Cli::parse_from(["homeboy", "audit", "--baseline"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "audit");
        assert!(command.portable);
        assert_eq!(command.unsupported_reason, None);
        assert!(command.routing_policy.requires_extension_parity);
    }

    #[test]
    fn lab_command_with_ratchet_stays_portable_for_patch_capture() {
        let cli = Cli::parse_from(["homeboy", "audit", "--ratchet"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "audit");
        assert!(command.portable);
        assert_eq!(command.unsupported_reason, None);
        assert!(command.routing_policy.requires_extension_parity);
    }

    #[test]
    fn lab_command_preserves_local_only_contract_shape() {
        let cli = Cli::parse_from(["homeboy", "rig", "up", "demo"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "rig up");
        assert!(!command.portable);
        assert!(command.unsupported_reason.is_some());
        assert!(!command.routing_policy.requires_extension_parity);
    }

    #[test]
    fn strip_component_target_replaces_positional_with_path() {
        let args = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
            "sample-component".to_string(),
        ];

        let rewritten = strip_component_target_args(&args, "sample-component", "/src/sample");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "lint".to_string(),
                "--fix".to_string(),
                "--path".to_string(),
                "/src/sample".to_string(),
            ]
        );
    }

    #[test]
    fn strip_component_target_replaces_component_flag_with_path() {
        let args = vec![
            "homeboy".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "lint".to_string(),
            "--write".to_string(),
            "--component".to_string(),
            "sample-component".to_string(),
        ];

        let rewritten = strip_component_target_args(&args, "sample-component", "/src/sample");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "lint".to_string(),
                "--write".to_string(),
                "--path".to_string(),
                "/src/sample".to_string(),
            ]
        );
    }

    #[test]
    fn strip_component_target_only_strips_first_positional_match() {
        // A `--from` value equal to the component id must survive; only the bare
        // positional component token is dropped.
        let args = vec![
            "homeboy".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "lint".to_string(),
            "--write".to_string(),
            "dmc".to_string(),
        ];

        let rewritten = strip_component_target_args(&args, "dmc", "/src/dmc");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "lint".to_string(),
                "--write".to_string(),
                "--path".to_string(),
                "/src/dmc".to_string(),
            ]
        );
    }

    #[test]
    fn strip_component_target_preserves_passthrough_args() {
        let args = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
            "dmc".to_string(),
            "--".to_string(),
            "dmc".to_string(),
        ];

        let rewritten = strip_component_target_args(&args, "dmc", "/src/dmc");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "lint".to_string(),
                "--fix".to_string(),
                "--".to_string(),
                "dmc".to_string(),
                "--path".to_string(),
                "/src/dmc".to_string(),
            ]
        );
    }

    #[test]
    fn rewrite_component_target_skips_when_path_override_present() {
        let cli = Cli::parse_from([
            "homeboy",
            "lint",
            "--fix",
            "sample-component",
            "--path",
            "/explicit/path",
        ]);
        let normalized = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
            "sample-component".to_string(),
            "--path".to_string(),
            "/explicit/path".to_string(),
        ];

        assert!(rewrite_component_target_to_path(&cli.command, &normalized).is_none());
    }

    #[test]
    fn rewrite_component_target_skips_without_component() {
        // No positional component and no --path: source resolves from CWD, so
        // there is nothing to rewrite.
        let cli = Cli::parse_from(["homeboy", "lint", "--fix"]);
        let normalized = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
        ];

        assert!(rewrite_component_target_to_path(&cli.command, &normalized).is_none());
    }

    #[test]
    fn changed_scope_source_path_uses_shared_target_resolution() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();

        let source_path = resolve_changed_scope_source_path(None, Some(&path))
            .expect("path override should resolve through TargetSpec");

        assert_eq!(source_path, path);
    }

    #[test]
    fn lab_route_source_path_args_keeps_component_id_without_patch_capture() {
        let cli = Cli::parse_from(["homeboy", "lint", "sample-component"]);
        let normalized = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "sample-component".to_string(),
        ];

        assert!(lab_route_source_path_args(&cli.command, &normalized, false).is_none());
    }

    #[test]
    fn rewrite_ad_hoc_lab_workspace_adds_path_for_pathless_lint() {
        let dir = tempdir().unwrap();
        let _cwd = CwdGuard::set(dir.path());
        let cli = Cli::parse_from(["homeboy", "lint"]);
        let normalized = vec!["homeboy".to_string(), "lint".to_string()];

        let rewritten = rewrite_ad_hoc_lab_workspace_to_path(&cli.command, &normalized)
            .expect("pathless lint should become explicit path");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "lint".to_string(),
                "--path".to_string(),
                dir.path().to_string_lossy().to_string(),
            ]
        );
    }

    #[test]
    fn rewrite_ad_hoc_lab_workspace_inserts_path_before_passthrough() {
        let dir = tempdir().unwrap();
        let _cwd = CwdGuard::set(dir.path());
        let cli = Cli::parse_from(["homeboy", "test", "--", "--filter", "ExampleTest"]);
        let normalized = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--".to_string(),
            "--filter".to_string(),
            "ExampleTest".to_string(),
        ];

        let rewritten = rewrite_ad_hoc_lab_workspace_to_path(&cli.command, &normalized)
            .expect("pathless test should become explicit path");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "test".to_string(),
                "--path".to_string(),
                dir.path().to_string_lossy().to_string(),
                "--".to_string(),
                "--filter".to_string(),
                "ExampleTest".to_string(),
            ]
        );
    }

    #[test]
    fn rewrite_ad_hoc_lab_workspace_skips_registered_component_or_path() {
        let component_cli = Cli::parse_from(["homeboy", "lint", "homeboy"]);
        let path_cli = Cli::parse_from(["homeboy", "audit", "--path", "/tmp/homeboy"]);

        assert!(rewrite_ad_hoc_lab_workspace_to_path(
            &component_cli.command,
            &[
                "homeboy".to_string(),
                "lint".to_string(),
                "homeboy".to_string(),
            ],
        )
        .is_none());
        assert!(rewrite_ad_hoc_lab_workspace_to_path(
            &path_cli.command,
            &[
                "homeboy".to_string(),
                "audit".to_string(),
                "--path".to_string(),
                "/tmp/homeboy".to_string(),
            ],
        )
        .is_none());
    }
}
