//! `homeboy workflow`: declarative multi-step and repeating work.
//!
//! A definition (`homeboy/workflow/v1`) is admitted as one daemon `workflow`
//! job. The daemon submits `step` jobs and evaluates gates; these verbs only
//! admit, observe, cancel, and approve. A definition with `every` is
//! registered as a cadence instead: the daemon admits one instance per due
//! slot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, CommandFactory, Subcommand};
use homeboy::core::daemon::workflow_job::{
    WorkflowJobRequest, WORKFLOW_INSTANCE_ENV, WORKFLOW_JOB_REQUEST_SCHEMA,
};
use homeboy::core::daemon::LocalControllerJobClient;
use homeboy::core::env_materialization_plan::{EnvMaterializationPlan, EnvSecretRef};
use homeboy::core::workflow::definition::StepKind;
use homeboy::core::workflow::WorkflowDefinition;
use homeboy::core::{Error, Result};
use serde_json::{json, Value};

use super::CmdResult;

use super::runs::workflow::{exit_code, watch_instance};

mod ab;
pub(crate) use ab::render_summary as render_ab_summary;

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
    /// Admit a workflow instance; next_pass forwards inputs with overrides, a typed iteration,
    /// previous outputs, max_iterations and optional stop_unchanged. Changed file definitions are refused.
    /// The same definition and inputs return the existing instance
    Run(RunArgs),
    /// Cancel an instance and every step it is running, or stop a registered cadence
    Cancel {
        /// Instance id, or the workflow id of a registered cadence
        instance: String,
    },
    /// Approve a step that is awaiting approval in the current iteration
    Approve {
        /// Instance id
        instance: String,
        /// Step id
        step: String,
    },
    /// Reject an awaiting approval, optionally providing feedback
    Reject {
        instance: String,
        step: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// List registered definitions, built-ins, and daemon cadences
    List,
    /// Prove a refactor byte-identical: run one command at two refs in isolated
    /// checkouts, concurrently, and compare the declared outputs (`homeboy.ab`)
    Ab(ab::AbArgs),
    /// A `homeboy.ab` recipe step (side, verdict, cleanup)
    #[command(hide = true)]
    AbStep(ab::AbStepArgs),
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
    /// Reuse unchanged succeeded steps from a terminal instance
    #[arg(long, value_name = "INSTANCE")]
    resume_from: Option<String>,
    /// Admit the workflow on this runner's daemon instead of the local one
    #[arg(long = "on", value_name = "RUNNER")]
    on: Option<String>,
    /// Set from the global `--wait`: observe the instance until it is
    /// terminal, then follow its child instances to the last one.
    #[arg(skip)]
    pub(crate) wait: bool,
}

impl WorkflowArgs {
    /// Project the global `--wait` onto `workflow run` and `workflow ab`.
    pub(crate) fn absorb_wait(&mut self, wait: bool) {
        match &mut self.command {
            WorkflowCommand::Run(run) => run.wait = wait,
            WorkflowCommand::Ab(ab) => ab.wait = wait,
            _ => {}
        }
    }

    /// Project the global `--placement` onto `workflow ab`.
    pub(crate) fn absorb_placement(&mut self, placement: crate::cli_surface::Placement) {
        if let WorkflowCommand::Ab(ab) = &mut self.command {
            ab.placement = placement;
        }
    }

    /// `workflow ab` selects its own runner and admits its recipe there; the
    /// front end itself always runs on this controller.
    pub(crate) fn is_ab(&self) -> bool {
        matches!(self.command, WorkflowCommand::Ab(_))
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
                        "foreach": step.foreach.is_some(),
                    })).collect::<Vec<_>>(),
                }),
                0,
            ))
        }
        WorkflowCommand::Run(run) => run_workflow(run),
        WorkflowCommand::Ab(args) => ab::run(args),
        WorkflowCommand::AbStep(args) => ab::step(args),
        WorkflowCommand::Cancel { instance } => {
            let client = LocalControllerJobClient::connect()?;
            let view = match client.workflow(&instance) {
                Ok(view) => view,
                // Not an instance: a registered cadence's workflow id.
                Err(error) => {
                    return client
                        .cancel_workflow_cadence(&instance)
                        .map(|cadence| (json!({ "cadence": cadence }), 0))
                        .map_err(|_| error);
                }
            };
            let job_id = view["job"]["job_id"]
                .as_str()
                .ok_or_else(|| Error::internal_unexpected("runs show has no job id"))?;
            client.cancel(job_id, &format!("workflow `{instance}` cancelled"))?;
            Ok((client.workflow(&instance)?, 0))
        }
        WorkflowCommand::Approve { instance, step } => {
            let client = LocalControllerJobClient::connect()?;
            Ok((client.approve_workflow_step(&instance, &step)?, 0))
        }
        WorkflowCommand::Reject {
            instance,
            step,
            reason,
        } => {
            let client = LocalControllerJobClient::connect()?;
            Ok((
                client.reject_workflow_step(&instance, &step, reason.as_deref())?,
                0,
            ))
        }
        WorkflowCommand::List => {
            let mut listed = registered_workflows()?;
            listed["cadences"] =
                LocalControllerJobClient::connect()?.workflow_cadences()?["entities"].clone();
            Ok((listed, 0))
        }
    }
}

fn run_workflow(args: RunArgs) -> CmdResult<Value> {
    let (definition, source) = load(&args.workflow)?;
    validate(&definition)?;
    if definition.every.is_some()
        && (args.wait || args.instance.is_some() || args.resume_from.is_some())
    {
        return Err(Error::validation_invalid_argument(
            "every",
            "a workflow with `every` is registered as a cadence; each slot names its own instance, so `--wait`, `--instance`, and `--resume-from` do not apply",
            None,
            None,
        ));
    }
    if let Some(runner) = &args.on {
        return run_on_runner(runner, &definition, &args);
    }
    if definition.every.is_some() {
        return register_cadence(&definition, source, &args);
    }
    let provided = args
        .inputs
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect::<BTreeMap<_, _>>();
    admit_with_resume(
        &definition,
        &provided,
        args.instance.as_deref(),
        args.wait,
        &source,
        args.resume_from.as_deref(),
    )
}

/// Admit `definition` on the local daemon, or report the instance the same
/// definition, inputs, and name already admitted.
pub(crate) fn admit(
    definition: &WorkflowDefinition,
    provided: &BTreeMap<String, Value>,
    instance_name: Option<&str>,
    wait: bool,
    source: &str,
) -> CmdResult<Value> {
    admit_with_resume(definition, provided, instance_name, wait, source, None)
}

fn admit_with_resume(
    definition: &WorkflowDefinition,
    provided: &BTreeMap<String, Value>,
    instance_name: Option<&str>,
    wait: bool,
    source: &str,
    resumed_from: Option<&str>,
) -> CmdResult<Value> {
    let inputs = definition.resolve_inputs(provided)?;
    let instance = definition.instance_id_with_resume(&inputs, instance_name, resumed_from);
    let client = LocalControllerJobClient::connect_current_build()?;
    // The same definition and inputs name the same instance: report it.
    let disposition = match client.workflow(&instance) {
        Ok(_) => "reused",
        Err(_) => {
            if let Some(source) = resumed_from {
                let view = client.workflow(source)?;
                if !matches!(
                    view["status"].as_str(),
                    Some("succeeded" | "failed" | "cancelled" | "budget_exhausted")
                ) {
                    return Err(Error::validation_invalid_argument(
                        "resume-from",
                        "resume source must be terminal",
                        None,
                        None,
                    ));
                }
            }
            let mut request = admission_request(
                definition,
                inputs,
                &instance,
                instance_name,
                parent_from_env(),
                current_dir()?,
            )?;
            if std::path::Path::new(source).is_file() {
                request.definition_source = Some(
                    std::fs::canonicalize(source)
                        .map_err(|e| Error::internal_io(e.to_string(), None))?
                        .display()
                        .to_string(),
                );
            }
            request.resumed_from = resumed_from.map(str::to_string);
            client.submit(request.submission()?)?;
            "created"
        }
    };
    let mut view = if wait {
        watch_instance(&client, &instance, std::time::Duration::from_secs(2), None)?.0
    } else {
        client.workflow(&instance)?
    };
    view["submission"] = json!({ "disposition": disposition, "source": source });
    view["status_command"] = json!(format!("homeboy runs show {instance}"));
    view["cancel_command"] = json!(format!("homeboy workflow cancel {instance}"));
    let code = if wait { exit_code(&view) } else { 0 };
    Ok((view, code))
}

/// `workflow run` on a definition with `every`: register its admitted request
/// as the daemon's cadence template.
fn register_cadence(
    definition: &WorkflowDefinition,
    source: String,
    args: &RunArgs,
) -> CmdResult<Value> {
    let provided = args
        .inputs
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect::<BTreeMap<_, _>>();
    let inputs = definition.resolve_inputs(&provided)?;
    let instance = definition.instance_id(&inputs, None);
    let request = admission_request(definition, inputs, &instance, None, None, current_dir()?)?;
    let client = LocalControllerJobClient::connect_current_build()?;
    let mut view = json!({ "cadence": client.register_workflow_cadence(&request)? });
    view["submission"] = json!({ "disposition": view["cadence"]["disposition"], "source": source });
    view["status_command"] = json!("homeboy workflow list");
    Ok((view, 0))
}

/// The admitted request the daemon registers for a built-in cadence, captured
/// from the daemon's own environment and run from its home directory.
pub fn register_builtin_cadence_factory() {
    homeboy::core::daemon::workflow_cadence::register_builtin_request_factory(std::sync::Arc::new(
        |definition: &WorkflowDefinition| {
            let inputs = definition.resolve_inputs(&BTreeMap::new())?;
            let instance = definition.instance_id(&inputs, None);
            let cwd = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_dir())
                .map(Ok)
                .unwrap_or_else(current_dir)?;
            admission_request(definition, inputs, &instance, None, None, cwd)
        },
    ));
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve the working directory".into()),
        )
    })
}

/// Capture the admitted environment and pin every program the steps run.
/// `parent` is the instance whose step is admitting this one, read from the
/// step environment `workflow run` runs in.
fn admission_request(
    definition: &WorkflowDefinition,
    inputs: BTreeMap<String, Value>,
    instance: &str,
    instance_name: Option<&str>,
    parent: Option<String>,
    cwd: PathBuf,
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
    for step in &definition.steps {
        match step.kind() {
            StepKind::Exec(exec) => {
                programs.insert(exec.program.clone(), resolve_program(&exec.program)?);
            }
            StepKind::Cook(_) | StepKind::Homeboy(_) => runs_homeboy = true,
            StepKind::Approval(_) => {}
        }
    }
    // `homeboy` and `cook` steps run Homeboy itself, with the same admitted
    // runtime environment a detached Cook step gets.
    let homeboy_env = if runs_homeboy {
        homeboy::agents::agent_task_service::admitted_cook_environment(Vec::new())?
    } else {
        EnvMaterializationPlan::default()
    };
    // The installed path, even when the running binary was replaced: a
    // cadence keeps running this program after upgrades.
    let current_exe = homeboy::core::daemon::daemon_launchable_executable().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve the Homeboy executable".into()),
        )
    })?;
    Ok(WorkflowJobRequest {
        schema: WORKFLOW_JOB_REQUEST_SCHEMA.to_string(),
        instance: instance.to_string(),
        instance_name: instance_name.map(str::to_string),
        resumed_from: None,
        definition_source: None,
        definition_digest: definition.digest(),
        definition: definition.clone(),
        inputs,
        homeboy_program: current_exe.display().to_string(),
        cwd: cwd.display().to_string(),
        parent,
        env,
        homeboy_env,
        programs,
        notification_route: homeboy::core::notification_route::current(),
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
    if let Some(source) = &args.resume_from {
        remote.push("--resume-from".to_string());
        remote.push(source.clone());
    }
    if let Some(route) = homeboy::core::notification_route::current() {
        remote.extend([
            "--notification-transport".to_string(),
            route.transport,
            "--notification-route".to_string(),
            route.route,
        ]);
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
    let first_json = |text: &str| -> Option<Value> {
        text.find('{')
            .and_then(|start| serde_json::from_str(&text[start..]).ok())
    };
    let mut envelope = first_json(&stdout);
    // `runner exec` may wrap the remote command's stdout in its own envelope.
    if let Some(remote) = envelope
        .as_ref()
        .filter(|envelope| envelope["command"] == "runner")
        .and_then(|envelope| {
            ["/data/stdout", "/data/presentation/stdout"]
                .iter()
                .filter_map(|pointer| envelope.pointer(pointer).and_then(Value::as_str))
                .find_map(first_json)
        })
    {
        envelope = Some(remote);
    }
    let envelope = envelope.ok_or_else(|| {
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
    let remote_exit = envelope["exit_code"].as_i64();
    let mut data = envelope.get("data").cloned().unwrap_or(envelope);
    data["remote_exit_code"] = json!(remote_exit);
    data["runner"] = json!(runner);
    data["status_command"] = json!(match data["instance"].as_str() {
        Some(instance) => format!("homeboy runner exec {runner} -- homeboy runs show {instance}"),
        None => format!("homeboy runner exec {runner} -- homeboy workflow list"),
    });
    let code = envelope_exit_code(&data, output.status.code());
    Ok((data, code))
}

fn envelope_exit_code(data: &Value, process: Option<i32>) -> i32 {
    data["remote_exit_code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok())
        .or(process)
        .unwrap_or(1)
}

/// The instance whose step runs this command, if any. A step's environment
/// names its instance, so `workflow run` inside a step records that instance
/// as the admitted one's parent and the chain stays visible in `status`.
fn parent_from_env() -> Option<String> {
    std::env::var(WORKFLOW_INSTANCE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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
        if !path.is_file() {
            if let Some(definition) = homeboy::core::workflow::builtin(workflow) {
                return Ok((definition, "builtin".to_string()));
            }
        }
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

pub(crate) fn validate(definition: &WorkflowDefinition) -> Result<()> {
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
                    "every": definition.every,
                }),
                Err(error) => json!({
                    "path": path.display().to_string(),
                    "valid": false,
                    "error": error,
                }),
            });
        }
    }
    for definition in homeboy::core::workflow::builtins() {
        workflows.push(json!({
            "id": definition.id,
            "builtin": true,
            "valid": true,
            "steps": definition.steps.len(),
            "every": definition.every,
        }));
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
            validate(&parsed.unwrap()).unwrap_or_else(|error| {
                panic!(
                    "docs example `{}` is invalid: {}",
                    value["id"], error.message
                )
            });
            examples += 1;
        }
        assert!(examples >= 3);
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
            assert_eq!(rows.len(), 4);
            assert_eq!(rows[0]["valid"], false);
            assert_eq!(rows[1]["id"], "ok");
            assert_eq!(rows[2]["id"], "homeboy.retention");
            assert_eq!(rows[2]["builtin"], true);
            assert_eq!(rows[3]["id"], "homeboy.ab");
        });
    }
}
