//! `homeboy workflow`: declarative multi-step and repeating work.
//!
//! A definition (`homeboy/workflow/v1`) is admitted as one daemon `workflow`
//! job. The daemon submits `step` jobs and evaluates gates; these verbs only
//! admit, observe, cancel, and approve.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Args, CommandFactory, Subcommand};
use homeboy::core::daemon::workflow_job::{WorkflowJobRequest, WORKFLOW_JOB_REQUEST_SCHEMA};
use homeboy::core::daemon::LocalControllerJobClient;
use homeboy::core::env_materialization_plan::{EnvMaterializationPlan, EnvSecretRef};
use homeboy::core::workflow::definition::StepKind;
use homeboy::core::workflow::WorkflowDefinition;
use homeboy::core::{Error, Result};
use serde_json::{json, Value};

use super::CmdResult;

const WATCH_POLL: Duration = Duration::from_secs(2);

#[derive(Args)]
pub struct WorkflowArgs {
    #[command(subcommand)]
    command: WorkflowCommand,
}

#[derive(Subcommand)]
enum WorkflowCommand {
    /// Validate a definition: file path, registered id, or `-` for stdin
    Validate {
        /// Definition file, registered workflow id, or `-`
        workflow: String,
    },
    /// Admit a workflow instance; the same definition and inputs return the existing instance
    Run(RunArgs),
    /// Show one instance (or all) as a projection of the daemon job ledger
    Status {
        /// Instance id (`wf-<id>-<hash>`); omit to list every instance
        instance: Option<String>,
        /// Poll until the instance is terminal
        #[arg(long)]
        watch: bool,
    },
    /// Cancel an instance and every step it is running
    Cancel {
        /// Instance id
        instance: String,
    },
    /// Approve a step that is awaiting approval in the current iteration
    Approve {
        /// Instance id
        instance: String,
        /// Step id
        step: String,
    },
    /// List instances, or registered definitions with `--registered`
    List {
        /// List definitions in the workflow registry instead of instances
        #[arg(long)]
        registered: bool,
    },
}

#[derive(Args)]
pub struct RunArgs {
    /// Definition file, registered workflow id, or `-` for stdin
    workflow: String,
    /// Workflow input as NAME=VALUE (repeatable)
    #[arg(long = "input", value_name = "NAME=VALUE", value_parser = super::parse_key_val)]
    inputs: Vec<(String, String)>,
    /// Instance name; distinct names run the same definition and inputs separately
    #[arg(long, value_name = "NAME")]
    instance: Option<String>,
    /// Admit the workflow on this runner's daemon instead of the local one
    #[arg(long = "on", value_name = "RUNNER")]
    on: Option<String>,
    /// Set from the global `--wait`: observe the instance until it is terminal.
    #[arg(skip)]
    pub(crate) wait: bool,
}

impl WorkflowArgs {
    /// Project the global `--wait` onto `workflow run`.
    pub(crate) fn absorb_wait(&mut self, wait: bool) {
        if let WorkflowCommand::Run(run) = &mut self.command {
            run.wait = wait;
        }
    }
}

pub fn run(args: WorkflowArgs) -> CmdResult<Value> {
    match args.command {
        WorkflowCommand::Validate { workflow } => {
            let (definition, source) = load(&workflow)?;
            validate(&definition)?;
            Ok((
                json!({
                    "valid": true,
                    "source": source,
                    "id": definition.id,
                    "digest": definition.digest(),
                    "inputs": definition.inputs,
                    "steps": definition.steps.iter().map(|step| json!({
                        "id": step.id,
                        "kind": kind_name(step.kind()),
                        "needs": step.needs,
                    })).collect::<Vec<_>>(),
                }),
                0,
            ))
        }
        WorkflowCommand::Run(run) => run_workflow(run),
        WorkflowCommand::Status { instance, watch } => {
            let client = LocalControllerJobClient::connect()?;
            let Some(instance) = instance else {
                return Ok((client.workflow_list()?, 0));
            };
            let view = if watch {
                watch_instance(&client, &instance)?
            } else {
                client.workflow(&instance)?
            };
            let code = exit_code(&view);
            Ok((view, code))
        }
        WorkflowCommand::Cancel { instance } => {
            let client = LocalControllerJobClient::connect()?;
            let view = client.workflow(&instance)?;
            let job_id = view["job"]["job_id"]
                .as_str()
                .ok_or_else(|| Error::internal_unexpected("workflow status has no job id"))?;
            client.cancel(job_id, &format!("workflow `{instance}` cancelled"))?;
            Ok((client.workflow(&instance)?, 0))
        }
        WorkflowCommand::Approve { instance, step } => {
            let client = LocalControllerJobClient::connect()?;
            Ok((client.approve_workflow_step(&instance, &step)?, 0))
        }
        WorkflowCommand::List { registered } => {
            if registered {
                return Ok((registered_workflows()?, 0));
            }
            Ok((LocalControllerJobClient::connect()?.workflow_list()?, 0))
        }
    }
}

fn run_workflow(args: RunArgs) -> CmdResult<Value> {
    let (definition, source) = load(&args.workflow)?;
    validate(&definition)?;
    if let Some(runner) = &args.on {
        return run_on_runner(runner, &definition, &args);
    }
    let provided = args
        .inputs
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect::<BTreeMap<_, _>>();
    let inputs = definition.resolve_inputs(&provided)?;
    let instance = definition.instance_id(&inputs, args.instance.as_deref());
    let client = LocalControllerJobClient::connect_current_build()?;
    // The same definition and inputs name the same instance: report it.
    let disposition = match client.workflow(&instance) {
        Ok(_) => "reused",
        Err(_) => {
            let request = admission_request(&definition, inputs, &instance, &args)?;
            client.submit(request.submission()?)?;
            "created"
        }
    };
    let mut view = if args.wait {
        watch_instance(&client, &instance)?
    } else {
        client.workflow(&instance)?
    };
    view["submission"] = json!({ "disposition": disposition, "source": source });
    view["status_command"] = json!(format!("homeboy workflow status {instance}"));
    let code = if args.wait { exit_code(&view) } else { 0 };
    Ok((view, code))
}

/// Capture the admitted environment and pin every program the steps run.
fn admission_request(
    definition: &WorkflowDefinition,
    inputs: BTreeMap<String, Value>,
    instance: &str,
    args: &RunArgs,
) -> Result<WorkflowJobRequest> {
    let mut env = EnvMaterializationPlan::default();
    for name in &definition.env.public {
        let value = std::env::var(name).map_err(|_| {
            Error::validation_invalid_argument(
                "env.public",
                format!("declared environment `{name}` is not set where the workflow is admitted"),
                Some(name.clone()),
                None,
            )
        })?;
        env.public_env.insert(name.clone(), value);
    }
    env.secret_refs = definition
        .env
        .secrets
        .iter()
        .map(|name| EnvSecretRef {
            name: name.clone(),
            owner: None,
        })
        .collect();
    env.normalize();

    let mut programs = BTreeMap::new();
    let mut runs_homeboy = false;
    let mut gate_programs = Vec::new();
    for step in &definition.steps {
        match step.kind() {
            StepKind::Exec(exec) => {
                programs.insert(exec.program.clone(), resolve_program(&exec.program)?);
            }
            StepKind::Cook(_) | StepKind::Homeboy(_) => runs_homeboy = true,
            StepKind::Approval(_) => {}
        }
        if let Some(gate) = &step.when {
            gate_programs.extend(gate.command_gates().into_iter().map(|argv| argv[0].clone()));
        }
    }
    if let Some(iterate) = &definition.iterate {
        gate_programs.extend(
            iterate
                .until
                .command_gates()
                .into_iter()
                .map(|argv| argv[0].clone()),
        );
    }
    for program in gate_programs {
        if program == "homeboy" {
            runs_homeboy = true;
        } else if program.contains("{{") {
            return Err(Error::validation_invalid_argument(
                "command",
                "a command gate's program cannot be templated",
                Some(program),
                None,
            ));
        } else {
            programs.insert(program.clone(), resolve_program(&program)?);
        }
    }
    // `homeboy` and `cook` steps run Homeboy itself, with the same admitted
    // runtime environment a detached Cook step gets.
    let homeboy_env = if runs_homeboy {
        homeboy::agents::agent_task_service::admitted_cook_environment(Vec::new())?
    } else {
        EnvMaterializationPlan::default()
    };
    let current_exe = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve the Homeboy executable".into()),
        )
    })?;
    let cwd = std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve the working directory".into()),
        )
    })?;
    Ok(WorkflowJobRequest {
        schema: WORKFLOW_JOB_REQUEST_SCHEMA.to_string(),
        instance: instance.to_string(),
        instance_name: args.instance.clone(),
        definition_digest: definition.digest(),
        definition: definition.clone(),
        inputs,
        homeboy_program: current_exe.display().to_string(),
        cwd: cwd.display().to_string(),
        env,
        homeboy_env,
        programs,
    })
}

fn resolve_program(program: &str) -> Result<String> {
    let path = Path::new(program);
    if path.is_absolute() {
        return Ok(program.to_string());
    }
    if program.contains('/') {
        let cwd = std::env::current_dir().unwrap_or_default();
        return Ok(cwd.join(path).display().to_string());
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
        .map(|candidate| candidate.display().to_string())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "program",
                format!("`{program}` is not on PATH where the workflow is admitted"),
                Some(program.to_string()),
                None,
            )
        })
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// `--on RUNNER`: admit the same definition on that runner's daemon through
/// `runner exec`, so its environment is admitted where it runs.
fn run_on_runner(
    runner: &str,
    definition: &WorkflowDefinition,
    args: &RunArgs,
) -> CmdResult<Value> {
    let mut remote = vec![
        "homeboy".to_string(),
        "workflow".to_string(),
        "run".to_string(),
        serde_json::to_string(definition).map_err(|error| {
            Error::internal_json(error.to_string(), Some("serialize workflow".into()))
        })?,
    ];
    for (name, value) in &args.inputs {
        remote.push("--input".to_string());
        remote.push(format!("{name}={value}"));
    }
    if let Some(instance) = &args.instance {
        remote.push("--instance".to_string());
        remote.push(instance.clone());
    }
    if args.wait {
        remote.push("--wait".to_string());
    }
    let program = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve the Homeboy executable".into()),
        )
    })?;
    let output = std::process::Command::new(program)
        .args(["runner", "exec", runner, "--raw", "--"])
        .args(&remote)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("runner exec {runner}")))
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = stdout
        .find('{')
        .and_then(|start| serde_json::from_str(&stdout[start..]).ok())
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "runner `{runner}` returned no workflow envelope: {}{}",
                stdout.trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        })?;
    if envelope["success"] == false {
        return Err(Error::internal_unexpected(format!(
            "runner `{runner}` refused the workflow: {}",
            envelope["error"]
        )));
    }
    let mut data = envelope.get("data").cloned().unwrap_or(envelope);
    data["runner"] = json!(runner);
    data["status_command"] = json!(format!(
        "homeboy runner exec {runner} -- homeboy workflow status {}",
        data["instance"].as_str().unwrap_or_default()
    ));
    let code = output.status.code().unwrap_or(1);
    Ok((data, code))
}

fn watch_instance(client: &LocalControllerJobClient, instance: &str) -> Result<Value> {
    let mut last = String::new();
    loop {
        let view = client.workflow(instance)?;
        let line = progress_line(&view);
        if line != last {
            eprintln!("{line}");
            last = line;
        }
        if is_terminal(&view) {
            return Ok(view);
        }
        std::thread::sleep(WATCH_POLL);
    }
}

fn progress_line(view: &Value) -> String {
    let steps = view["steps"]
        .as_object()
        .map(|steps| {
            steps
                .iter()
                .map(|(id, step)| {
                    format!(
                        "{id}={}",
                        step["outcome"]
                            .as_str()
                            .filter(|_| step["phase"] == "done")
                            .unwrap_or_else(|| step["phase"].as_str().unwrap_or("?"))
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    format!(
        "{} {} iteration {} runs {}: {steps}",
        view["instance"].as_str().unwrap_or_default(),
        view["status"].as_str().unwrap_or_default(),
        view["iteration"],
        view["step_runs"],
    )
}

fn is_terminal(view: &Value) -> bool {
    !matches!(view["status"].as_str(), Some("running" | "queued"))
}

fn exit_code(view: &Value) -> i32 {
    match view["status"].as_str() {
        Some("succeeded" | "running" | "queued") => 0,
        _ => 1,
    }
}

fn kind_name(kind: StepKind<'_>) -> &'static str {
    match kind {
        StepKind::Cook(_) => "cook",
        StepKind::Homeboy(_) => "homeboy",
        StepKind::Exec(_) => "exec",
        StepKind::Approval(_) => "approval",
    }
}

/// Load a definition from a file, the registry, stdin, or inline JSON.
fn load(workflow: &str) -> Result<(WorkflowDefinition, String)> {
    let (text, source) = if workflow == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).map_err(|error| {
            Error::internal_io(error.to_string(), Some("read workflow from stdin".into()))
        })?;
        (text, "stdin".to_string())
    } else if workflow.trim_start().starts_with('{') {
        (workflow.to_string(), "inline".to_string())
    } else {
        let path = PathBuf::from(workflow);
        let path = if path.is_file() {
            path
        } else {
            homeboy::core::workflow::registry_dir()?.join(format!("{workflow}.json"))
        };
        let text = std::fs::read_to_string(&path).map_err(|error| {
            Error::validation_invalid_argument(
                "workflow",
                format!("no workflow file or registered id `{workflow}`: {error}"),
                Some(workflow.to_string()),
                None,
            )
        })?;
        (text, path.display().to_string())
    };
    Ok((WorkflowDefinition::parse(&text)?, source))
}

fn validate(definition: &WorkflowDefinition) -> Result<()> {
    definition.validate(&cook_flag_exists)
}

/// Whether `key` is a real `agent-task cook` flag (or a global flag the Cook
/// accepts), read from the live clap tree so there is one Cook contract.
fn cook_flag_exists(key: &str) -> bool {
    let root = crate::cli_surface::Cli::command();
    let cook = root
        .find_subcommand("agent-task")
        .and_then(|agent_task| agent_task.find_subcommand("cook"));
    let has = |command: &clap::Command| {
        command
            .get_arguments()
            .any(|arg| arg.get_long() == Some(key) && !arg.is_hide_set())
    };
    cook.is_some_and(has) || has(&root)
}

fn registered_workflows() -> Result<Value> {
    let dir = homeboy::core::workflow::registry_dir()?;
    let mut workflows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut paths = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let parsed = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| WorkflowDefinition::parse(&text).map_err(|error| error.message))
                .and_then(|definition| {
                    validate(&definition)
                        .map(|_| definition)
                        .map_err(|error| error.message)
                });
            workflows.push(match parsed {
                Ok(definition) => json!({
                    "id": definition.id,
                    "path": path.display().to_string(),
                    "valid": true,
                    "steps": definition.steps.len(),
                }),
                Err(error) => json!({
                    "path": path.display().to_string(),
                    "valid": false,
                    "error": error,
                }),
            });
        }
    }
    Ok(json!({ "registry": dir.display().to_string(), "entities": workflows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cook_keys_are_checked_against_the_live_cook_flags() {
        for flag in [
            "repo",
            "prompt",
            "verify",
            "task-url",
            "backend",
            "placement",
        ] {
            assert!(cook_flag_exists(flag), "{flag}");
        }
        for flag in ["bogus", "cook-id"] {
            assert!(!cook_flag_exists(flag), "{flag}");
        }
    }

    #[test]
    fn documented_examples_validate() {
        let docs = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/commands/workflow.md"),
        )
        .unwrap();
        let mut examples = 0;
        for block in docs.split("```json\n").skip(1) {
            let json = block.split("```").next().unwrap();
            let value: Value = serde_json::from_str(json).expect("docs example is JSON");
            if value["schema"] != "homeboy/workflow/v1" {
                continue;
            }
            let parsed = WorkflowDefinition::from_value(value.clone());
            // Examples B (`every`, `notify`) and C (`foreach`) use fields a
            // later release implements; this one refuses them.
            if value["id"] == "cook-batch" || value["id"] == "homeboy-nightly-red-main" {
                assert!(parsed.is_err());
                continue;
            }
            validate(&parsed.unwrap()).unwrap_or_else(|error| {
                panic!(
                    "docs example `{}` is invalid: {}",
                    value["id"], error.message
                )
            });
            examples += 1;
        }
        assert!(examples >= 2);
    }

    #[test]
    fn registered_list_returns_entity_rows() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let dir = homeboy::core::workflow::registry_dir().unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("ok.json"),
                r#"{"schema":"homeboy/workflow/v1","id":"ok","steps":[{"id":"a","exec":{"program":"true"}}]}"#,
            )
            .unwrap();
            std::fs::write(dir.join("bad.json"), "{").unwrap();
            let listed = registered_workflows().unwrap();
            let rows = listed["entities"].as_array().unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0]["valid"], false);
            assert_eq!(rows[1]["id"], "ok");
        });
    }
}
