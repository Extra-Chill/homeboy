use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::command_execution_plan::CommandSourceMaterialization;
use homeboy::core::component::{self, TargetSpec};
use homeboy::core::git;
use homeboy::core::lab_routing::{
    self, LabDispatchObserver, LabRouteOutcome, LabRoutingRequest, NoopLabDispatchObserver,
    PersistedRunRetrieval,
};
use homeboy::core::observation::{
    finish_run_best_effort, NewRunRecord, ObservationStore, RunStatus,
};
use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::runners::{self, RunnerExecOptions};
use homeboy::core::Error;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::io::output_file::write_output_file;

pub fn route_after_parse(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    if lab_routing::is_lab_offload_subprocess() {
        return Ok(None);
    }

    if cli.runner.is_none() && crate::commands::utils::resource_policy::is_runner_hosted_exec() {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Runs(args)) = (cli.runner.as_deref(), &cli.command) {
        if !is_runs_list_runner_option(normalized_args) && !args.has_command_local_runner_option() {
            return Err(crate::commands::runs::global_runner_error(args, runner_id));
        }

        return Ok(None);
    }

    if is_command_local_runner_option(&cli.command) {
        return Ok(None);
    }

    if let (Some(runner_id), Commands::Rig(args)) = (cli.runner.as_deref(), &cli.command) {
        if let Some(rig_id) = args.up_dry_run_rig_id() {
            let (output, exit_code) = crate::commands::rig::up_runner_exec_plan(rig_id, runner_id)?;
            let stdout = serde_json::to_string_pretty(&output).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("serialize rig up runner exec plan".to_string()),
                )
            })?;
            if let Some(path) = output_file {
                write_output_file(path, &stdout)?;
            }
            println!("{stdout}");
            return Ok(Some(exit_code));
        }
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

    let inferred_runner_id = if lab_command.is_some() {
        cli.runner
            .clone()
            .or_else(|| runners::resolve_default_lab_runner().ok().flatten())
    } else {
        None
    };

    let observer = lab_dispatch_observer(cli, normalized_args, inferred_runner_id.as_deref());
    let active_run_id = observer.run_id().map(str::to_string);

    let capture_mutation_patch = cli.command.lab_offload_captures_mutation_patch();
    let mutation_flag = cli.command.lab_offload_mutation_flag();

    // For component-targeted write/fix commands (`homeboy review lint --fix <component>`,
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
            skip_deps_hydration: cli.skip_deps_hydration,
            capture_patch: capture_mutation_patch,
            mutation_flag,
            timeout: lab_route_dispatch_timeout(&cli.command, cli.detach_after_handoff),
            active_run_id: active_run_id.as_deref(),
            detach_after_handoff: cli.detach_after_handoff,
            output_file_requested: output_file.is_some(),
            read_only_polling: cli
                .command
                .lab_route_contract()?
                .is_some_and(|contract| contract.command.routing_policy.read_only_polling),
            local_output_file: output_file,
            job_overrides,
        },
        inferred_runner_id.as_deref(),
        observer,
    )?;

    match outcome {
        LabRouteOutcome::RunLocal => {
            if destructive_fuzz_requires_lab(&cli.command) {
                return Err(destructive_fuzz_local_execution_error());
            }
            if let Some(warning) = agent_task_local_fanout_warning(&cli.command) {
                eprintln!("{warning}");
            }
            Ok(None)
        }
        LabRouteOutcome::InFlight(output) | LabRouteOutcome::Offloaded(output) => {
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

fn lab_route_dispatch_timeout(
    command: &Commands,
    detach_after_handoff: bool,
) -> Option<std::time::Duration> {
    if matches!(command, Commands::Trace(_)) {
        return Some(lab_routing::lab_trace_dispatch_timeout());
    }
    if detach_after_handoff && is_agent_task_fanout_cook_batch_run_plan(command) {
        return Some(lab_routing::lab_trace_dispatch_timeout());
    }
    None
}

/// Insert one env pair into the overrides, recording the key as secret when
/// the redaction policy considers the key sensitive or the value redacted.
fn insert_lab_env_override(
    overrides: &mut runners::LabJobOverrides,
    policy: &RedactionPolicy,
    name: String,
    value: String,
) {
    if policy.is_sensitive_key(&name) || policy.redact_string(&value) != value {
        overrides.secret_env_names.push(name.clone());
    }
    overrides.env.insert(name, value);
}

fn lab_job_overrides(cli: &Cli) -> homeboy::core::Result<runners::LabJobOverrides> {
    let mut overrides = runners::LabJobOverrides::default();
    let policy = RedactionPolicy::default();

    for raw in &cli.runner_env {
        let (name, value) = parse_lab_env_pair("runner-env", raw)?;
        insert_lab_env_override(&mut overrides, &policy, name, value);
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
            insert_lab_env_override(&mut overrides, &policy, name, value);
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
    let Commands::Review(args) = command else {
        return Ok(None);
    };
    let Some(component_args) = args.lab_changed_scope_component_args() else {
        return Ok(None);
    };
    if has_lab_changed_files_json(normalized_args) {
        return Ok(None);
    }

    let target = component::resolve_target(TargetSpec::new(
        component_args.component.as_deref(),
        component_args.path.as_deref(),
    ))?;
    let source_path = target.source_path.to_string_lossy();
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

fn has_lab_changed_files_json(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--lab-changed-files-json" || arg.starts_with("--lab-changed-files-json=")
    })
}

/// Build the Lab dispatch observer for the parsed command. Only `trace`
/// participates in dispatch observation; every other command uses the no-op
/// observer. The core routing service owns the observation lifecycle; this
/// adapter only supplies the implementation.
fn lab_dispatch_observer(
    cli: &Cli,
    normalized_args: &[String],
    runner_id: Option<&str>,
) -> Box<dyn LabDispatchObserver> {
    match &cli.command {
        Commands::Trace(args) => {
            crate::commands::trace::start_lab_dispatch_observation(args, normalized_args, runner_id)
                .map(|observation| Box::new(observation) as Box<dyn LabDispatchObserver>)
                .unwrap_or_else(|| Box::new(NoopLabDispatchObserver))
        }
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command:
                crate::commands::agent_task::AgentTaskCommand::Fanout(
                    crate::commands::agent_task::AgentTaskFanoutArgs {
                        command:
                            crate::commands::agent_task::AgentTaskFanoutCommand::CookBatch(args),
                    },
                ),
        }) if cli.detach_after_handoff && args.run_plan => {
            start_agent_task_fanout_lab_dispatch_observation(args, normalized_args, runner_id)
                .map(|observation| Box::new(observation) as Box<dyn LabDispatchObserver>)
                .unwrap_or_else(|| Box::new(NoopLabDispatchObserver))
        }
        _ => Box::new(NoopLabDispatchObserver),
    }
}

struct AgentTaskFanoutLabDispatchObservation {
    store: ObservationStore,
    run_id: String,
    fanout_id: String,
}

impl LabDispatchObserver for AgentTaskFanoutLabDispatchObservation {
    fn run_id(&self) -> Option<&str> {
        Some(self.run_id.as_str())
    }

    fn finish(
        self: Box<Self>,
        status: RunStatus,
        metadata: serde_json::Value,
    ) -> Option<PersistedRunRetrieval> {
        let metadata =
            agent_task_fanout_finish_metadata(metadata, &self.run_id, &self.fanout_id, status);
        finish_run_best_effort(&self.store, &self.run_id, status, Some(metadata));
        Some(PersistedRunRetrieval::for_run(&self.run_id))
    }
}

fn agent_task_fanout_finish_metadata(
    mut metadata: serde_json::Value,
    run_id: &str,
    fanout_id: &str,
    status: RunStatus,
) -> serde_json::Value {
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "agent_task_lab_dispatch".to_string(),
            serde_json::json!({
                "schema": "homeboy/agent-task-fanout-lab-dispatch/v1",
                "fanout_id": fanout_id,
                "phase": "route_lab_dispatch",
                "status": status.as_str(),
            }),
        );
        object.insert(
            "fanout_id".to_string(),
            serde_json::Value::String(fanout_id.to_string()),
        );
        object.insert(
            "follow_commands".to_string(),
            serde_json::json!({
                "dispatch_status": format!("homeboy runs show {run_id}"),
                "dispatch_evidence": format!("homeboy runs evidence --run {run_id}"),
                "fanout_status": format!("homeboy agent-task fanout status {fanout_id}"),
            }),
        );
    }
    metadata
}

fn start_agent_task_fanout_lab_dispatch_observation(
    args: &crate::commands::agent_task::AgentTaskFanoutCookBatchArgs,
    normalized_args: &[String],
    runner_id: Option<&str>,
) -> Option<AgentTaskFanoutLabDispatchObservation> {
    let store = ObservationStore::open_initialized().ok()?;
    let cwd = std::env::current_dir().ok();
    let fanout_id = agent_task_fanout_cook_batch_dispatch_id(args);
    let run = store
        .start_run(
            NewRunRecord::builder("agent-task")
                .component_id(args.repo.clone())
                .command(normalized_args.join(" "))
                .optional_cwd_path(cwd.as_deref())
                .current_homeboy_version()
                .metadata(serde_json::json!({
                    "agent_task_lab_dispatch": {
                        "schema": "homeboy/agent-task-fanout-lab-dispatch/v1",
                        "fanout_id": fanout_id,
                        "phase": "route_before_lab_dispatch",
                        "status": "running",
                        "runner_id": runner_id,
                        "detach_after_handoff": true,
                        "run_plan": true,
                        "issue_count": args.issues.len(),
                    },
                    "runner_id": runner_id,
                    "fanout_id": fanout_id,
                    "follow_commands": {
                        "fanout_status": format!("homeboy agent-task fanout status {}", fanout_id),
                    },
                }))
                .build(),
        )
        .ok()?;
    eprintln!(
        "Lab offload handoff: local dispatch run `{}` is durable before remote preflight; inspect dispatch with `homeboy runs show {}`. Once the fanout batch is submitted, inspect it with `homeboy agent-task fanout status {}`.",
        run.id, run.id, fanout_id
    );
    Some(AgentTaskFanoutLabDispatchObservation {
        store,
        run_id: run.id,
        fanout_id,
    })
}

fn agent_task_fanout_cook_batch_dispatch_id(
    args: &crate::commands::agent_task::AgentTaskFanoutCookBatchArgs,
) -> String {
    args.fanout_id.clone().unwrap_or_else(|| {
        let first = args
            .issues
            .first()
            .and_then(|issue| issue.split_once("/issues/").map(|(_, number)| number))
            .and_then(|number| number.split(|c| matches!(c, '/' | '?' | '#')).next())
            .filter(|value| !value.is_empty())
            .unwrap_or("batch");
        format!(
            "cook-batch-{}-issue-{}-{}",
            args.repo,
            first,
            args.issues.len()
        )
    })
}

fn is_agent_task_fanout_cook_batch_run_plan(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command:
                crate::commands::agent_task::AgentTaskCommand::Fanout(
                    crate::commands::agent_task::AgentTaskFanoutArgs {
                        command:
                            crate::commands::agent_task::AgentTaskFanoutCommand::CookBatch(args),
                    },
                ),
        }) if args.run_plan
    )
}

fn run_rig_source_management_on_runner(
    runner_id: &str,
    normalized_args: &[String],
    output_file: Option<&str>,
) -> homeboy::core::Result<(String, String, i32)> {
    let runner = runners::load(runner_id)?;
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let mut command = runner_rig_source_management_command(homeboy_path, normalized_args);
    let mut translated_remote_root = runner.workspace_root.clone().unwrap_or_default();
    let local_cwd = std::env::current_dir().ok();
    if let Some(local_cwd) = local_cwd.as_deref() {
        if command_contains_path_prefix(&command, local_cwd) {
            let (synced, sync_exit_code) = runners::sync_workspace(
                runner_id,
                runners::RunnerWorkspaceSyncOptions {
                    path: local_cwd.display().to_string(),
                    mode: runners::RunnerWorkspaceSyncMode::Snapshot,
                    ..Default::default()
                },
            )?;
            if sync_exit_code == 0 {
                command = translate_command_path_prefix(&command, local_cwd, &synced.remote_path);
                translated_remote_root = synced.remote_path;
            }
        }
    }

    // `rig install <source>` forwards the controller-local rig package path. When
    // that package lives outside the synced working directory the runner cannot
    // see it and fails with `Path does not exist: /Users/...`, so the documented
    // one-command `rig install --runner` offload is broken without a local
    // pre-install + `--skip-install` (#6964). Materialize the source's containing
    // checkout on the runner and translate the forwarded path through the same
    // sync+translate seam the working directory already uses. Syncing the
    // containing checkout (not just the package directory) keeps rigs that
    // declare `package_dependencies` — which resolve against the repo root — and
    // package-level `extends` templates working on the runner.
    let rig_install_source_root = rig_install_source_sync_root(&command);
    if let Some(source_root) = rig_install_source_root.as_deref() {
        let (synced, sync_exit_code) = runners::sync_workspace(
            runner_id,
            runners::RunnerWorkspaceSyncOptions {
                path: source_root.display().to_string(),
                mode: runners::RunnerWorkspaceSyncMode::Snapshot,
                ..Default::default()
            },
        )?;
        if sync_exit_code == 0 {
            command = translate_command_path_prefix(&command, source_root, &synced.remote_path);
            translated_remote_root = synced.remote_path;
        }
    }

    // Remote-execution preflight before dispatching caller-derived argv to the
    // runner (#5093):
    // 1. Path-translation: reject any forwarded argument that still embeds the
    //    controller-local working directory or rig-install source path instead of
    //    the runner-resident workspace, so a controller-only path never reaches
    //    the remote runtime.
    // 2. Capability parity: validate the runner can run the forwarded `homeboy`
    //    binary before execution starts (enforced by `runners::exec` against the
    //    supplied `RunnerCapabilityPreflight`).
    if let Some(local_cwd) = local_cwd.as_deref() {
        runners::preflight_remote_argv_path_translation(
            "Rig source management",
            runner_id,
            &command,
            local_cwd,
            &translated_remote_root,
        )?;
    }
    if let Some(source_root) = rig_install_source_root.as_deref() {
        runners::preflight_remote_argv_path_translation(
            "Rig source management",
            runner_id,
            &command,
            source_root,
            &translated_remote_root,
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
            secret_env_plan: None,
            env_materialization: None,
            capture_patch: false,
            raw_exec: false,
            source_snapshot: None,
            path_materialization_plan: None,
            capability_preflight,
            required_extensions: Vec::new(),
            accepted_extension_settings: Vec::new(),
            require_paths: Vec::new(),
            runner_workload: None,
            run_id: None,
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;

    if let Some(path) = output_file {
        write_offloaded_stdout(path, &output.stdout)?;
    }

    Ok((output.stdout, output.stderr, exit_code))
}

fn command_contains_path_prefix(command: &[String], local_root: &Path) -> bool {
    let local_root = local_root.display().to_string();
    let local_root = local_root.trim_end_matches('/');
    !local_root.is_empty() && command.iter().any(|arg| arg.contains(local_root))
}

fn translate_command_path_prefix(
    command: &[String],
    local_root: &Path,
    remote_root: &str,
) -> Vec<String> {
    let local_root = local_root.display().to_string();
    let local_root = local_root.trim_end_matches('/');
    let remote_root = remote_root.trim_end_matches('/');
    command
        .iter()
        .map(|arg| {
            if local_root.is_empty() || remote_root.is_empty() {
                arg.clone()
            } else {
                arg.replace(local_root, remote_root)
            }
        })
        .collect()
}

/// Resolve the controller-local sync root for a `rig install <source>` argument
/// that still references a path on this controller after working-directory
/// translation. Returns the source's containing git checkout (or the path
/// itself when it is not inside a git repo) so the caller can materialize it on
/// the runner and translate the forwarded argument (#6964).
///
/// Returns `None` for git-URL sources, paths already translated to the runner,
/// and any source that does not exist on this controller — leaving non-path and
/// already-remote arguments untouched.
fn rig_install_source_sync_root(command: &[String]) -> Option<PathBuf> {
    let source = rig_install_source_arg(command)?;
    let expanded = shellexpand::tilde(&source).to_string();
    let path = Path::new(&expanded);
    if !path.exists() {
        return None;
    }
    let canonical = path.canonicalize().ok()?;
    Some(homeboy::core::git::repo_root(&canonical).unwrap_or(canonical))
}

/// Return the positional `<source>` of a `rig install` command, skipping the
/// flags the subcommand accepts (`--id <value>`, `--id=<value>`, `--all`,
/// `--reinstall`/`--force`). The command argv is `[homeboy, rig, install, ...]`
/// with controller-only globals already stripped by
/// [`runner_rig_source_management_command`].
fn rig_install_source_arg(command: &[String]) -> Option<String> {
    let install_index = command
        .windows(2)
        .position(|window| window[0] == "rig" && window[1] == "install")?
        + 2;
    let mut iter = command[install_index..].iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            return iter.next().cloned();
        }
        if arg == "--id" {
            iter.next();
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.clone());
    }
    None
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
            || arg == "--lab-only"
            || arg == "--no-local-execution"
            || arg == "--force-hot"
            || arg == "--detach-after-handoff"
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
    match command {
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::Doctor(_),
        }) => true,
        Commands::Fuzz(args) => args.consumes_runner_as_plan_input(),
        _ => false,
    }
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

fn destructive_fuzz_requires_lab(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Fuzz(args) if args.destructive_local_execution_requires_override()
    )
}

fn destructive_fuzz_local_execution_error() -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "allow_local_destructive_fuzz",
        "destructive fuzz refused local controller execution".to_string(),
        Some("--allow-destructive".to_string()),
        Some(vec![
            "Omit --force-hot/--allow-local-hot or pass --runner <runner-id> to run destructive fuzz on Lab.".to_string(),
            "Configure a default Lab runner so destructive fuzz offloads automatically.".to_string(),
            "If local execution is absolutely intentional, pass --allow-local-destructive-fuzz together with --allow-destructive.".to_string(),
        ]),
    )
}

fn lab_route_source_path_args(
    command: &Commands,
    normalized_args: &[String],
    capture_mutation_patch: bool,
) -> Option<Vec<String>> {
    if capture_mutation_patch || command_prefers_controller_source_path(command) {
        if let Some(rewritten) = rewrite_component_target_to_path(command, normalized_args) {
            return Some(rewritten);
        }
    }

    rewrite_ad_hoc_lab_workspace_to_path(command, normalized_args)
}

fn command_prefers_controller_source_path(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Review(crate::commands::review::ReviewArgs {
            command: Some(crate::commands::review::ReviewCommand::Lint(_)),
            ..
        })
    )
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
        Commands::Refactor(args) if args.is_hot_resource_command() => (
            args.lab_offload_positional_component(),
            args.lab_offload_has_path_override(),
        ),
        Commands::Review(args) => match &args.command {
            Some(crate::commands::review::ReviewCommand::Lint(lint_args)) => (
                lint_args.lab_offload_positional_component(),
                lint_args.lab_offload_has_path_override(),
            ),
            _ => return None,
        },
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
    let contract = command.lab_contract()?;
    let plan = lab_routing::lab_route_plan_from_contract(contract);
    if plan.source_materialization != CommandSourceMaterialization::ControllerCwdAsPathArg {
        return None;
    }

    let needs_path = matches!(
        command,
        Commands::Review(args)
            if args.nested_component_args().is_some_and(|component| {
                component.component.is_none() && component.path.is_none()
            })
    );
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
        if rewritten.is_empty() {
            rewritten.push(arg.clone());
            continue;
        }
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
        previous: Vec<(&'static str, Option<String>)>,
        _guard: MutexGuard<'static, ()>,
    }

    struct CwdGuard {
        previous: std::path::PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            Self::set_many(&[(name, Some(value))])
        }

        fn remove(name: &'static str) -> Self {
            Self::set_many(&[(name, None)])
        }

        fn set_many(changes: &[(&'static str, Option<&str>)]) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let mut previous = Vec::with_capacity(changes.len());
            for (name, value) in changes {
                previous.push((*name, std::env::var(name).ok()));
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            Self {
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
            for (name, previous) in self.previous.iter().rev() {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
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

    fn write_command_only_rig(home: &Path, rig_id: &str) {
        let rigs_dir = home.join(".config").join("homeboy").join("rigs");
        fs::create_dir_all(&rigs_dir).expect("create rigs dir");
        let spec = serde_json::json!({
            "id": rig_id,
            "description": "command-only rig",
            "pipeline": {
                "up": [
                    {
                        "kind": "command",
                        "command": "./scripts/run-matrix.sh",
                        "cwd": "tools",
                        "env": { "MATRIX": "portable" },
                        "label": "run matrix"
                    }
                ]
            }
        });
        fs::write(
            rigs_dir.join(format!("{rig_id}.json")),
            serde_json::to_string_pretty(&spec).expect("serialize rig"),
        )
        .expect("write rig");
    }

    #[test]
    fn non_lab_command_continues_local_dispatch() {
        let cli = Cli::parse_from(["homeboy", "status"]);

        let outcome = route_after_parse(&cli, &["homeboy".into(), "status".into()], None).unwrap();

        assert_eq!(outcome, None);
    }

    #[test]
    fn changed_scope_lint_is_lab_portable() {
        let cli = Cli::parse_from([
            "homeboy",
            "review",
            "lint",
            "--changed-since",
            "origin/main",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "review lint");
        assert!(command.portable);
        assert!(command.unsupported_reason.is_none());
    }

    #[test]
    fn nested_review_quality_subcommands_use_specific_lab_labels() {
        for (args, expected_label) in [
            (
                vec!["homeboy", "review", "audit", "data-machine"],
                "review audit",
            ),
            (
                vec!["homeboy", "review", "lint", "data-machine"],
                "review lint",
            ),
            (
                vec!["homeboy", "review", "test", "data-machine"],
                "review test",
            ),
            (
                vec!["homeboy", "review", "build", "data-machine"],
                "review build",
            ),
            (
                vec![
                    "homeboy",
                    "review",
                    "ci",
                    "run",
                    "data-machine",
                    "--job",
                    "lint",
                ],
                "review ci",
            ),
        ] {
            let cli = Cli::parse_from(args);
            let command = cli.command.lab_contract().unwrap();

            assert_eq!(command.hot_label, expected_label);
        }
    }

    #[test]
    fn nested_review_lint_dispatch_uses_matching_lab_label() {
        let cli = Cli::parse_from(["homeboy", "review", "lint", "data-machine"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "review lint");
        assert!(command.portable);
        assert!(command.unsupported_reason.is_none());
    }

    #[test]
    fn nested_review_quality_subcommand_resolves_effective_component() {
        let cli = Cli::parse_from(["homeboy", "review", "lint", "data-machine"]);
        let Commands::Review(args) = cli.command else {
            panic!("expected review command");
        };

        assert_eq!(
            args.effective_component_args().component.as_deref(),
            Some("data-machine")
        );
    }

    #[test]
    fn nested_review_quality_in_dir_offload_uses_current_dir_path() {
        let dir = tempdir().expect("tempdir");
        let _cwd = CwdGuard::set(dir.path());
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
            "lint".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let rewritten = lab_route_source_path_args(&cli.command, &normalized, false)
            .expect("review lint without component gets cwd path rewrite");
        let cwd = std::env::current_dir().expect("current dir");

        assert_eq!(rewritten[0..3], normalized);
        assert_eq!(rewritten[3], "--path");
        assert_eq!(rewritten[4], cwd.to_string_lossy());
    }

    #[test]
    fn explicit_runner_for_changed_scope_test_is_lab_portable() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "review",
            "test",
            "--changed-since",
            "origin/main",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert_eq!(command.hot_label, "review test");
        assert!(command.portable);
        assert!(command.unsupported_reason.is_none());
    }

    #[test]
    fn destructive_fuzz_local_execution_requires_explicit_destructive_local_override() {
        let normalized = vec![
            "homeboy",
            "--force-hot",
            "--allow-local-hot",
            "fuzz",
            "run",
            "component-a",
            "--allow-destructive",
            "--isolation",
            "isolated",
            "--isolation-proof",
            "proof.json",
        ];
        let cli = Cli::parse_from(&normalized);

        assert!(destructive_fuzz_requires_lab(&cli.command));

        let error = crate::test_support::with_isolated_home(|_| {
            route_after_parse(
                &cli,
                &normalized
                    .iter()
                    .map(|arg| arg.to_string())
                    .collect::<Vec<_>>(),
                None,
            )
            .expect_err("destructive fuzz local route should be refused")
        });
        assert!(error
            .to_string()
            .contains("destructive fuzz refused local controller execution"));
    }

    #[test]
    fn destructive_fuzz_local_override_is_command_specific_and_explicit() {
        let cli = Cli::parse_from([
            "homeboy",
            "--force-hot",
            "--allow-local-hot",
            "fuzz",
            "run",
            "component-a",
            "--allow-destructive",
            "--allow-local-destructive-fuzz",
            "--isolation",
            "isolated",
            "--isolation-proof",
            "proof.json",
        ]);

        assert!(!destructive_fuzz_requires_lab(&cli.command));
    }

    #[test]
    fn rig_up_dry_run_with_runner_emits_runner_exec_plan() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        crate::test_support::with_isolated_home(|home| {
            runners::create(
                r#"{"id":"homeboy-lab","kind":"local","homeboy_path":"/runner/bin/homeboy-patched"}"#,
                false,
            )
            .expect("runner");
            write_command_only_rig(home.path(), "script-matrix");
            let output = home.path().join("plan.json");
            let normalized = vec![
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "rig".to_string(),
                "up".to_string(),
                "script-matrix".to_string(),
                "--dry-run".to_string(),
            ];
            let cli = Cli::parse_from(&normalized);

            let outcome = route_after_parse(&cli, &normalized, Some(&output.to_string_lossy()))
                .expect("route rig up plan");

            assert_eq!(outcome, Some(0));
            let plan: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(output).expect("read output plan"))
                    .expect("parse output plan");
            assert_eq!(plan["variant"], "up_plan");
            assert_eq!(plan["payload"]["runner_id"], "homeboy-lab");
            assert_eq!(
                plan["payload"]["selected_homeboy_binary"],
                "/runner/bin/homeboy-patched"
            );
            assert_eq!(
                plan["payload"]["commands"][0],
                "/runner/bin/homeboy-patched runner exec homeboy-lab --cwd tools --env MATRIX=portable -- sh -c ./scripts/run-matrix.sh"
            );
        });
    }

    #[test]
    fn lab_job_overrides_parse_env_json_and_workspace_root() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner-env",
            "STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH=/tmp/sample-runtime",
            "--runner-env",
            "API_TOKEN=secret-token",
            "--lab-env-json",
            r#"{"EXTRA_PATH":"/tmp/extra","EMPTY":null}"#,
            "--runner-workspace-root",
            "/srv/job-workspace",
            "review",
            "test",
            "studio-native",
        ]);

        let overrides = lab_job_overrides(&cli).expect("overrides");

        assert_eq!(
            overrides.env["STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH"],
            "/tmp/sample-runtime"
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
        let cli = Cli::parse_from(["homeboy", "--runner-env", "NO_EQUALS", "review"]);
        let err = lab_job_overrides(&cli).expect_err("invalid pair");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");

        let cli = Cli::parse_from(["homeboy", "--lab-env-json", "[]", "review"]);
        let err = lab_job_overrides(&cli).expect_err("invalid json object");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn changed_since_lint_keeps_git_scope_for_lab_runner() {
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
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
            "review".to_string(),
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
    fn runner_hosted_bench_exec_skips_recursive_lab_routing_without_explicit_runner() {
        let _env = EnvGuard::set_many(&[
            (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
            (homeboy::core::runner::RUNNER_HOSTED_EXEC_ENV, Some("1")),
        ]);
        let normalized = vec![
            "homeboy".to_string(),
            "--allow-local-hot".to_string(),
            "bench".to_string(),
            "--extension".to_string(),
            "wordpress".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("runner-hosted bench execution should stay local");

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
    fn lab_route_dispatch_timeout_plumbs_core_timeout() {
        let trace_cli = Cli::parse_from(["homeboy", "trace", "list"]);
        let lint_cli = Cli::parse_from(["homeboy", "review", "lint"]);

        assert_eq!(
            lab_route_dispatch_timeout(&trace_cli.command, false),
            Some(lab_routing::lab_trace_dispatch_timeout())
        );
        assert_eq!(lab_route_dispatch_timeout(&lint_cli.command, false), None);
    }

    #[test]
    fn detached_agent_task_fanout_cook_batch_run_plan_uses_bounded_handoff_timeout() {
        let cli = Cli::parse_from([
            "homeboy",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--verify",
            "cargo test --lib",
            "--run-plan",
            "https://github.com/Extra-Chill/homeboy/issues/7167",
        ]);

        assert_eq!(
            lab_route_dispatch_timeout(&cli.command, cli.detach_after_handoff),
            Some(lab_routing::lab_trace_dispatch_timeout())
        );

        let no_detach = Cli::parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--verify",
            "cargo test --lib",
            "--run-plan",
            "https://github.com/Extra-Chill/homeboy/issues/7167",
        ]);
        assert_eq!(lab_route_dispatch_timeout(&no_detach.command, false), None);
    }

    #[test]
    fn agent_task_fanout_dispatch_id_uses_explicit_or_stable_default() {
        let cli = Cli::parse_from([
            "homeboy",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--fanout-id",
            "wave-7167",
            "--verify",
            "cargo test --lib",
            "--run-plan",
            "https://github.com/Extra-Chill/homeboy/issues/7167",
        ]);
        let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command:
                crate::commands::agent_task::AgentTaskCommand::Fanout(
                    crate::commands::agent_task::AgentTaskFanoutArgs {
                        command:
                            crate::commands::agent_task::AgentTaskFanoutCommand::CookBatch(args),
                    },
                ),
        }) = cli.command
        else {
            panic!("cook-batch command");
        };

        assert_eq!(agent_task_fanout_cook_batch_dispatch_id(&args), "wave-7167");

        let mut default_args = args;
        default_args.fanout_id = None;
        assert_eq!(
            agent_task_fanout_cook_batch_dispatch_id(&default_args),
            "cook-batch-homeboy-issue-7167-1"
        );
    }

    #[test]
    fn agent_task_fanout_finish_metadata_preserves_discoverability_commands() {
        let metadata = agent_task_fanout_finish_metadata(
            serde_json::json!({
                "lab_dispatch": {
                    "status": "error",
                    "runner_id": "homeboy-lab",
                },
            }),
            "dispatch-run-7167",
            "cook-batch-homeboy-issue-7167-1",
            RunStatus::Error,
        );

        assert_eq!(
            metadata["agent_task_lab_dispatch"]["fanout_id"],
            "cook-batch-homeboy-issue-7167-1"
        );
        assert_eq!(metadata["agent_task_lab_dispatch"]["status"], "error");
        assert_eq!(
            metadata["follow_commands"]["dispatch_status"],
            "homeboy runs show dispatch-run-7167"
        );
        assert_eq!(
            metadata["follow_commands"]["dispatch_evidence"],
            "homeboy runs evidence --run dispatch-run-7167"
        );
        assert_eq!(
            metadata["follow_commands"]["fanout_status"],
            "homeboy agent-task fanout status cook-batch-homeboy-issue-7167-1"
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
            "--lab-only".to_string(),
            "--force-hot".to_string(),
            "--detach-after-handoff".to_string(),
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
    fn runner_rig_source_management_translates_local_subdir_paths() {
        let command = vec![
            "/runner/bin/homeboy".to_string(),
            "rig".to_string(),
            "install".to_string(),
            "/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer".to_string(),
        ];

        let translated = translate_command_path_prefix(
            &command,
            std::path::Path::new("/Users/chubes/Developer/homeboy-rigs@run"),
            "/home/chubes/Developer/_lab_workspaces/homeboy-rigs-run-abc",
        );

        assert_eq!(
            translated[3],
            "/home/chubes/Developer/_lab_workspaces/homeboy-rigs-run-abc/WordPress/static-site-importer"
        );
    }

    #[test]
    fn rig_install_source_arg_finds_positional_source_after_flags() {
        let command = vec![
            "/runner/bin/homeboy".to_string(),
            "rig".to_string(),
            "install".to_string(),
            "--id".to_string(),
            "static-site-importer".to_string(),
            "--reinstall".to_string(),
            "/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer".to_string(),
            "--all".to_string(),
        ];

        assert_eq!(
            rig_install_source_arg(&command).as_deref(),
            Some("/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer")
        );
    }

    #[test]
    fn rig_install_source_arg_ignores_non_install_commands() {
        let command = vec![
            "/runner/bin/homeboy".to_string(),
            "rig".to_string(),
            "sources".to_string(),
            "list".to_string(),
        ];

        assert_eq!(rig_install_source_arg(&command), None);
    }

    #[test]
    fn rig_install_source_sync_root_resolves_existing_local_package() {
        let source_dir = tempdir().expect("source dir");
        let source_path = source_dir
            .path()
            .canonicalize()
            .expect("canonical temp dir")
            .join("static-site-importer");
        fs::create_dir_all(&source_path).expect("create source package");
        let command = vec![
            "/runner/bin/homeboy".to_string(),
            "rig".to_string(),
            "install".to_string(),
            source_path.to_string_lossy().to_string(),
        ];

        let sync_root = rig_install_source_sync_root(&command).expect("sync root");

        // The temp dir is not a git repo, so the package directory itself is the
        // materialization root.
        assert_eq!(sync_root, source_path);
    }

    #[test]
    fn rig_install_source_sync_root_skips_git_url_and_missing_paths() {
        let git_url = vec![
            "/runner/bin/homeboy".to_string(),
            "rig".to_string(),
            "install".to_string(),
            "https://github.com/Extra-Chill/homeboy-rigs.git".to_string(),
        ];
        assert_eq!(rig_install_source_sync_root(&git_url), None);

        let missing = vec![
            "/runner/bin/homeboy".to_string(),
            "rig".to_string(),
            "install".to_string(),
            "/Users/chubes/Developer/does-not-exist-rig-package-6964".to_string(),
        ];
        assert_eq!(rig_install_source_sync_root(&missing), None);
    }

    #[test]
    fn rig_install_offload_translates_source_path_instead_of_forwarding_it() {
        let source_dir = tempdir().expect("source dir");
        let source_path = source_dir
            .path()
            .canonicalize()
            .expect("canonical temp dir")
            .join("static-site-importer");
        fs::create_dir_all(&source_path).expect("create source package");
        let local_source = source_path.to_string_lossy().to_string();
        let command = vec![
            "/runner/bin/homeboy".to_string(),
            "rig".to_string(),
            "install".to_string(),
            local_source.clone(),
            "--reinstall".to_string(),
        ];

        let sync_root = rig_install_source_sync_root(&command).expect("sync root");
        let remote_root = "/home/runner/Developer/_lab_workspaces/static-site-importer-abc";
        let translated = translate_command_path_prefix(&command, &sync_root, remote_root);

        // The forwarded source must be the runner-side path, never the
        // controller-local path that broke `rig install --runner` (#6964).
        assert_eq!(translated[3], remote_root);
        assert!(
            !translated.iter().any(|arg| arg.contains(&local_source)),
            "controller-local source path must not be forwarded: {translated:?}"
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
        let temp_home = tempdir().expect("temp home");
        let _env = EnvGuard::set_many(&[
            (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
            ("HOME", Some(temp_home.path().to_str().expect("home path"))),
        ]);
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
        let cli = Cli::parse_from(["homeboy", "review", "lint"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "review lint");
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

        assert_eq!(command.hot_label, "extension refresh");
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
    fn extension_show_routes_to_explicit_lab_runner() {
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "lab",
            "extension",
            "show",
            "wordpress",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "extension show");
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert!(command.unsupported_reason.is_none());
        assert!(!command.routing_policy.requires_extension_parity);
        assert!(command.required_extensions.is_empty());
        assert!(!command.routing_policy.infer_source_path_tools);
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn fuzz_doctor_supports_runner_lab_only_diagnostic_route() {
        let cli = Cli::parse_from([
            "homeboy",
            "fuzz",
            "doctor",
            "--extension",
            "nodejs",
            "--runner",
            "homeboy-lab",
            "--lab-only",
        ]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();
        let local_policy = runners::LabLocalExecutionPolicy::from_flags(
            cli.allow_local_hot,
            cli.allow_local_fallback,
            cli.lab_only,
        );

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(local_policy.deny_local_execution());
        assert_eq!(command.hot_label, "fuzz doctor");
        assert!(lab_runner_supports_contract_label(command.hot_label));
        assert!(command.portable);
        assert!(!command.routing_policy.default_lab_offload);
        assert!(command.routing_policy.requires_extension_parity);
        assert!(command.routing_policy.read_only_polling);
        assert_eq!(command.required_extensions, vec!["nodejs".to_string()]);
        assert_eq!(
            command.source_path_mode,
            runners::LabOffloadSourcePathMode::RunnerResident
        );
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::RunnerResident
        );
        assert!(cli.command.lab_offload_mutation_flag().is_none());
        assert!(!cli.command.lab_offload_captures_mutation_patch());
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn fuzz_doctor_routes_locally_without_explicit_lab_runner() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let normalized = vec![
            "homeboy".to_string(),
            "fuzz".to_string(),
            "doctor".to_string(),
            "--extension".to_string(),
            "nodejs".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse(&cli, &normalized, None)
            .expect("fuzz doctor without --runner should remain a local diagnostic");

        assert_eq!(outcome, None);
        assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
    }

    #[test]
    fn extension_list_stays_local_only() {
        let cli = Cli::parse_from(["homeboy", "--runner", "lab", "extension", "list"]);

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
    fn runs_artifact_attach_runner_option_routes_locally() {
        let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);

        for normalized in [
            vec![
                "homeboy".to_string(),
                "runs".to_string(),
                "artifact".to_string(),
                "attach".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "--path".to_string(),
                "/tmp/matrix-summary.json".to_string(),
                "--name".to_string(),
                "matrix-summary".to_string(),
                "run-123".to_string(),
            ],
            vec![
                "homeboy".to_string(),
                "runs".to_string(),
                "artifact".to_string(),
                "attach".to_string(),
                "--runner=homeboy-lab".to_string(),
                "--path=/tmp/matrix-summary.json".to_string(),
                "--name=matrix-summary".to_string(),
                "run-123".to_string(),
            ],
        ] {
            let cli = Cli::parse_from(&normalized);

            let outcome = route_after_parse(&cli, &normalized, None)
                .expect("runs artifact attach command-local runner option should not be rejected");

            assert_eq!(outcome, None);
        }
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
    fn agent_task_cook_supports_automatic_explicit_and_lab_only_routing() {
        let automatic = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "homeboy@cook-routing",
            "--verify",
            "cargo test --locked",
        ]);
        let automatic_command = lab_offload_command(&automatic.command).unwrap().unwrap();
        assert!(automatic_command.portable);
        assert!(automatic_command.routing_policy.default_lab_offload);

        let explicit = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "homeboy@cook-routing",
            "--verify",
            "cargo test --locked",
        ]);
        let explicit_command = lab_offload_command(&explicit.command).unwrap().unwrap();
        assert_eq!(explicit.runner.as_deref(), Some("homeboy-lab"));
        assert!(explicit_command.portable);

        let lab_only = Cli::parse_from([
            "homeboy",
            "--lab-only",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "homeboy@cook-routing",
            "--verify",
            "cargo test --locked",
        ]);
        let local_policy = runners::LabLocalExecutionPolicy::from_flags(
            lab_only.allow_local_hot,
            lab_only.allow_local_fallback,
            lab_only.lab_only,
        );
        assert!(
            lab_offload_command(&lab_only.command)
                .unwrap()
                .unwrap()
                .portable
        );
        assert!(local_policy.deny_local_execution());
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
        assert!(command.routing_policy.default_lab_offload);
        assert!(!command.routing_policy.requires_extension_parity);
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
        );
    }

    #[test]
    fn agent_task_controller_materialization_family_auto_selects_default_lab_runner() {
        for args in [
            [
                "homeboy",
                "agent-task",
                "controller",
                "from-spec",
                "loop.json",
                "--resume",
                "--max-actions",
                "1",
            ]
            .as_slice(),
            [
                "homeboy",
                "agent-task",
                "controller",
                "run-from-spec",
                "loop.json",
                "--max-actions",
                "1",
            ]
            .as_slice(),
            [
                "homeboy",
                "agent-task",
                "controller",
                "materialize",
                "loop.json",
            ]
            .as_slice(),
        ] {
            let cli = Cli::parse_from(args);

            let command = lab_offload_command(&cli.command).unwrap().unwrap();

            assert_eq!(
                command.hot_label,
                "agent-task controller from-spec --resume/run-from-spec/materialize"
            );
            assert!(command.portable);
            assert!(command.routing_policy.default_lab_offload);
            assert!(command.routing_policy.infer_source_path_tools);
            assert!(!command.routing_policy.requires_extension_parity);
            assert_eq!(
                command.workspace_mode_policy,
                runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
            );
        }
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
    fn agent_task_fanout_run_plan_supports_lab_runner_routing() {
        let normalized = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--lab-only".to_string(),
            "agent-task".to_string(),
            "fanout".to_string(),
            "run-plan".to_string(),
            "--input".to_string(),
            "fanout.json".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();
        let local_policy = runners::LabLocalExecutionPolicy::from_flags(
            cli.allow_local_hot,
            cli.allow_local_fallback,
            cli.lab_only,
        );

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(local_policy.deny_local_execution());
        assert_eq!(command.hot_label, "agent-task fanout run-plan");
        assert!(command.portable);
        assert!(command.routing_policy.default_lab_offload);
        assert!(command.routing_policy.requires_extension_parity);
    }

    #[test]
    fn agent_task_fanout_cook_batch_run_plan_supports_lab_runner_routing() {
        let normalized = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--lab-only".to_string(),
            "agent-task".to_string(),
            "fanout".to_string(),
            "cook-batch".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
            "--verify".to_string(),
            "cargo test --locked agent_task".to_string(),
            "--run-plan".to_string(),
            "https://github.com/Extra-Chill/homeboy/issues/7011".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();
        let local_policy = runners::LabLocalExecutionPolicy::from_flags(
            cli.allow_local_hot,
            cli.allow_local_fallback,
            cli.lab_only,
        );

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(local_policy.deny_local_execution());
        assert_eq!(command.hot_label, "agent-task fanout cook-batch");
        assert!(command.portable);
        assert!(command.routing_policy.default_lab_offload);
        assert!(command.routing_policy.requires_extension_parity);
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
        let cli = Cli::parse_from(["homeboy", "review", "audit", "--baseline"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "review audit");
        assert!(command.portable);
        assert_eq!(command.unsupported_reason, None);
        assert!(command.routing_policy.requires_extension_parity);
    }

    #[test]
    fn lab_command_with_ratchet_stays_portable_for_patch_capture() {
        let cli = Cli::parse_from(["homeboy", "review", "audit", "--ratchet"]);

        let command = lab_offload_command(&cli.command).unwrap().unwrap();

        assert_eq!(command.hot_label, "review audit");
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
            "review".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
            "sample-component".to_string(),
        ];

        let rewritten = strip_component_target_args(&args, "sample-component", "/src/sample");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "review".to_string(),
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
            "review",
            "lint",
            "--fix",
            "sample-component",
            "--path",
            "/explicit/path",
        ]);
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
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
        let cli = Cli::parse_from(["homeboy", "review", "lint", "--fix"]);
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
            "lint".to_string(),
            "--fix".to_string(),
        ];

        assert!(rewrite_component_target_to_path(&cli.command, &normalized).is_none());
    }

    #[test]
    fn lab_route_source_path_args_rewrites_review_lint_component_without_patch_capture() {
        let cli = Cli::parse_from(["homeboy", "review", "lint", "homeboy"]);
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
            "lint".to_string(),
            "homeboy".to_string(),
        ];

        let rewritten = lab_route_source_path_args(&cli.command, &normalized, false)
            .expect("review lint component id should become a source path");

        assert_eq!(rewritten[0..3], normalized[0..3]);
        assert_eq!(
            rewritten
                .iter()
                .filter(|arg| arg.as_str() == "homeboy")
                .count(),
            1
        );
        assert!(rewritten.contains(&"--path".to_string()));
    }

    #[test]
    fn rewrite_ad_hoc_lab_workspace_adds_path_for_pathless_lint() {
        let dir = tempdir().unwrap();
        let _cwd = CwdGuard::set(dir.path());
        let cwd = std::env::current_dir().expect("current dir");
        let cli = Cli::parse_from(["homeboy", "review", "lint"]);
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
            "lint".to_string(),
        ];

        let rewritten = rewrite_ad_hoc_lab_workspace_to_path(&cli.command, &normalized)
            .expect("pathless lint should become explicit path");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "lint".to_string(),
                "--path".to_string(),
                cwd.to_string_lossy().to_string(),
            ]
        );
    }

    #[test]
    fn rewrite_ad_hoc_lab_workspace_inserts_path_before_passthrough() {
        let dir = tempdir().unwrap();
        let _cwd = CwdGuard::set(dir.path());
        let cwd = std::env::current_dir().expect("current dir");
        let cli = Cli::parse_from(["homeboy", "review", "test", "--", "--filter", "ExampleTest"]);
        let normalized = vec![
            "homeboy".to_string(),
            "review".to_string(),
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
                "review".to_string(),
                "test".to_string(),
                "--path".to_string(),
                cwd.to_string_lossy().to_string(),
                "--".to_string(),
                "--filter".to_string(),
                "ExampleTest".to_string(),
            ]
        );
    }

    #[test]
    fn rewrite_ad_hoc_lab_workspace_skips_registered_component_or_path() {
        let component_cli = Cli::parse_from(["homeboy", "review", "lint", "homeboy"]);
        let path_cli = Cli::parse_from(["homeboy", "review", "audit", "--path", "/tmp/homeboy"]);

        assert!(rewrite_ad_hoc_lab_workspace_to_path(
            &component_cli.command,
            &[
                "homeboy".to_string(),
                "review".to_string(),
                "lint".to_string(),
                "homeboy".to_string(),
            ],
        )
        .is_none());
        assert!(rewrite_ad_hoc_lab_workspace_to_path(
            &path_cli.command,
            &[
                "homeboy".to_string(),
                "review".to_string(),
                "audit".to_string(),
                "--path".to_string(),
                "/tmp/homeboy".to_string(),
            ],
        )
        .is_none());
    }
}
