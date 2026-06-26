use homeboy::core::runners::{
    self as runner, ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput, RunnerExecOutput,
};
use homeboy::core::server::RunnerSettings;

use super::super::output_runtime::{CommandPresentation, JsonCommandRun};
use super::super::CmdResult;
use super::broker::run_broker;
use super::cli::{RunnerArgs, RunnerCommand};
use super::exec::exec;
use super::jobs::RunnerJobCommandOutput;
use super::registry::{add, connect, enable, list, remove, set, show, RunnerAddInput};
use super::types::{RunnerCommandOutput, RunnerEnvOutput, RunnerOutput};
use super::{doctor, env as env_mod, jobs, policy, registry, status as status_mod, workspace};

pub fn run(
    args: RunnerArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<RunnerCommandOutput> {
    match args.command {
        RunnerCommand::Add {
            json,
            skip_existing,
            id,
            kind,
            server,
            workspace_root,
            homeboy_path,
            daemon,
            concurrency_limit,
            artifact_policy,
        } => map_registry(add(RunnerAddInput {
            json,
            skip_existing,
            id,
            kind,
            server,
            workspace_root,
            settings: RunnerSettings {
                homeboy_path,
                daemon,
                concurrency_limit,
                artifact_policy,
            },
        })),
        RunnerCommand::Enable {
            server_id,
            workspace_root,
            homeboy_path,
            daemon,
            concurrency_limit,
            artifact_policy,
        } => map_registry(enable(
            &server_id,
            workspace_root,
            RunnerSettings {
                homeboy_path,
                daemon,
                concurrency_limit,
                artifact_policy,
            },
        )),
        RunnerCommand::List => map_registry(list()),
        RunnerCommand::Show { id } => map_registry(show(&id)),
        RunnerCommand::Set { args } => map_registry(set(args)),
        RunnerCommand::Trust {
            runner_id,
            projects,
            commands,
            allow_raw_exec,
            workspace_roots,
            artifact_policy,
            peers,
            fingerprints,
        } => map_registry(policy::update(
            &runner_id,
            policy::RunnerPolicyPatch::trust(
                peers,
                fingerprints,
                projects,
                commands,
                allow_raw_exec,
                workspace_roots,
                artifact_policy,
            ),
            "runner.trust",
        )),
        RunnerCommand::Pair {
            runner_id,
            peers,
            fingerprints,
            projects,
            workspace_roots,
            allow_raw_exec,
        } => map_registry(policy::update(
            &runner_id,
            policy::RunnerPolicyPatch::pair(
                peers,
                fingerprints,
                projects,
                allow_raw_exec,
                workspace_roots,
            ),
            "runner.pair",
        )),
        RunnerCommand::Remove { id } => map_registry(remove(&id)),
        RunnerCommand::Doctor {
            runner_id,
            path,
            required_extensions,
            required_tools,
            scope,
            repair,
        } => map_doctor(doctor::run_with_options(
            &runner_id,
            doctor::RunnerDoctorOptions {
                path,
                extensions: required_extensions,
                required_tools,
                scope: scope.into(),
                repair,
            },
        )),
        RunnerCommand::Connect {
            id,
            reverse,
            reverse_runner,
            broker_url,
        } => map_registry(connect(&id, reverse, reverse_runner, broker_url)),
        RunnerCommand::Status { id } => map_registry(status_mod::status(id.as_deref())),
        RunnerCommand::Disconnect { id } => map_registry(registry::disconnect(&id)),
        RunnerCommand::Exec {
            id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            dry_run,
            run_id,
            artifact_outputs,
            summary_outputs,
            raw: _,
            command,
        } => map_execution(exec(
            &id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            dry_run,
            run_id,
            artifact_outputs,
            summary_outputs,
            command,
        )),
        RunnerCommand::Env { id } => map_env(env_mod::env(&id)),
        RunnerCommand::Job { command } => map_job(jobs::job(command)),
        RunnerCommand::Work {
            runner_id,
            broker_url,
            broker_token,
            project,
            lease_ms,
            r#loop,
            idle_backoff_ms,
            max_idle_backoff_ms,
            broker_failure_backoff_ms,
            broker_retry_limit,
        } => {
            let concurrency_limit = runner::load(&runner_id)
                .ok()
                .and_then(|runner| runner.settings.concurrency_limit);
            let broker_token = broker_token.or_else(runner::broker_token_from_env);
            map_worker(runner::run_reverse_worker(ReverseRunnerWorkerOptions {
                runner_id,
                broker_url,
                broker_token,
                project_id: project,
                lease_ms,
                concurrency_limit,
                loop_mode: r#loop,
                idle_backoff_ms,
                max_idle_backoff_ms,
                broker_failure_backoff_ms,
                broker_retry_limit,
            }))
        }
        RunnerCommand::Workspace { command } => workspace::run(command)
            .map(|(output, exit_code)| (RunnerCommandOutput::Workspace(output), exit_code)),
        RunnerCommand::Broker { command } => {
            run_broker(command).map(|output| (RunnerCommandOutput::Broker(output), 0))
        }
    }
}

pub fn run_command_output(args: RunnerArgs, _global: &super::super::GlobalArgs) -> JsonCommandRun {
    crate::commands::utils::tty::status("homeboy is working...");

    match args.command {
        RunnerCommand::Exec {
            id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            dry_run,
            run_id,
            artifact_outputs,
            summary_outputs,
            raw: true,
            command,
        } => run_raw_exec(
            id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            dry_run,
            run_id,
            artifact_outputs,
            summary_outputs,
            command,
        ),
        command => {
            let (stdout_result, exit_code) =
                crate::commands::utils::response::map_cmd_result_to_json(run(
                    RunnerArgs { command },
                    _global,
                ));
            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_raw_exec(
    id: String,
    cwd: Option<String>,
    project: Option<String>,
    ssh: bool,
    capture_patch: bool,
    require_paths: Vec<String>,
    script_file: Option<String>,
    env: Vec<String>,
    dry_run: bool,
    run_id: Option<String>,
    artifact_outputs: Vec<String>,
    summary_outputs: Vec<String>,
    command: Vec<String>,
) -> JsonCommandRun {
    match exec(
        &id,
        cwd,
        project,
        ssh,
        capture_patch,
        require_paths,
        script_file,
        env,
        dry_run,
        run_id,
        artifact_outputs,
        summary_outputs,
        command,
    ) {
        Ok((output, exit_code)) => raw_exec_command_run(output, exit_code),
        Err(err) => {
            let (stdout_result, exit_code) =
                crate::commands::utils::response::map_cmd_result_to_json::<RunnerCommandOutput>(
                    Err(err),
                );
            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

pub(super) fn raw_exec_command_run(output: RunnerExecOutput, exit_code: i32) -> JsonCommandRun {
    let presentation_stdout = output.stdout.clone();
    let presentation_stderr = output.stderr.clone();
    let (stdout_result, _) = crate::commands::utils::response::map_cmd_result_to_json(Ok((
        RunnerCommandOutput::Execution(output),
        exit_code,
    )));

    JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
        CommandPresentation {
            stdout: Some(presentation_stdout),
            stderr: Some(presentation_stderr),
        },
    )
}

pub(super) fn map_registry(result: CmdResult<RunnerOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(mut output, exit_code)| {
        registry::redact_runner_output_env(&mut output);
        output.extra.variant = runner_variant_from_command(&output.command);
        (RunnerCommandOutput::Registry(output), exit_code)
    })
}

fn runner_variant_from_command(command: &str) -> &'static str {
    match command {
        "runner.add" => "add",
        "runner.enable" => "enable",
        "runner.list" => "list",
        "runner.show" => "show",
        "runner.set" => "set",
        "runner.trust" => "trust",
        "runner.pair" => "pair",
        "runner.remove" => "remove",
        "runner.connect" => "connect",
        "runner.status" => "status",
        "runner.disconnect" => "disconnect",
        _ => "registry",
    }
}

fn map_doctor(result: CmdResult<doctor::RunnerDoctorOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Doctor(output), exit_code))
}

fn map_execution(result: CmdResult<RunnerExecOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Execution(output), exit_code))
}

fn map_env(result: CmdResult<RunnerEnvOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Env(output), exit_code))
}

fn map_job(result: CmdResult<RunnerJobCommandOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| match output {
        RunnerJobCommandOutput::Daemon(output) => (RunnerCommandOutput::Job(output), exit_code),
        RunnerJobCommandOutput::Broker(output) => {
            (RunnerCommandOutput::BrokerJob(output), exit_code)
        }
    })
}

fn map_worker(result: CmdResult<ReverseRunnerWorkerOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Worker(output), exit_code))
}
