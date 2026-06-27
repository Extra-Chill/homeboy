use std::path::Path;

use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::core::runners;
use homeboy::core::runners::RunnerWorkspaceSyncMode;

use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct LabArgs {
    #[command(subcommand)]
    command: LabCommand,
}

#[derive(Subcommand)]
enum LabCommand {
    /// Plan a runner-backed refresh loop before dispatching matrix-style work
    RefreshPlan(RefreshPlanArgs),
}

#[derive(Args, Debug, Clone)]
pub struct RefreshPlanArgs {
    /// Runner ID that will execute the workload
    #[arg(long)]
    runner: String,

    /// Controller-side workspace or worktree to sync to the runner
    #[arg(long = "workspace")]
    workspace: String,

    /// Runner-side cwd for the eventual runner exec command
    #[arg(long = "runner-cwd")]
    runner_cwd: String,

    /// Stable run id to use for the produced evidence
    #[arg(long = "run-id")]
    run_id: String,

    /// Produced output directory or file. Relative paths are resolved from --runner-cwd.
    #[arg(long = "output", value_name = "PATH")]
    outputs: Vec<String>,

    /// Produced summary directory or file. Relative paths are resolved from --runner-cwd.
    #[arg(long = "summary", value_name = "PATH")]
    summaries: Vec<String>,

    /// Source path that must exist before the refresh is dispatched. Repeat for multiple paths.
    #[arg(long = "source", value_name = "PATH")]
    sources: Vec<String>,

    /// Fixture path that must exist before the refresh is dispatched. Repeat for multiple paths.
    #[arg(long = "fixture", value_name = "PATH")]
    fixtures: Vec<String>,

    /// Runner workspace sync mode to use in the planned sync command.
    #[arg(long = "sync-mode", default_value = "snapshot")]
    sync_mode: String,

    /// Command and arguments to run after the plan checks pass.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanOutput {
    pub variant: &'static str,
    pub runner: String,
    pub workspace: String,
    pub runner_cwd: String,
    pub run_id: String,
    pub handoff: LabRefreshPlanHandoff,
    pub checks: Vec<LabRefreshPlanCheck>,
    pub evidence_paths: Vec<LabRefreshPlanEvidencePath>,
    pub next_commands: Vec<LabRefreshPlanCommand>,
    pub docs: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanHandoff {
    pub schema: &'static str,
    pub run_id: String,
    pub handoff_id: String,
    pub workload_id: String,
    pub runner: LabRefreshPlanRunnerHandoff,
    pub workspace: LabRefreshPlanWorkspaceHandoff,
    pub env_plan: LabRefreshPlanEnvPlan,
    pub secret_plan: LabRefreshPlanSecretPlan,
    pub runtime_refs: LabRefreshPlanRuntimeRefs,
    pub lifecycle: LabRefreshPlanLifecycle,
    pub artifact: LabRefreshPlanArtifactPlan,
    pub evidence: LabRefreshPlanEvidencePlan,
    pub result: LabRefreshPlanResultPlan,
    pub inspection: LabRefreshPlanInspectionPlan,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanRunnerHandoff {
    pub id: String,
    pub mode: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanWorkspaceHandoff {
    pub controller_path: String,
    pub runner_cwd: String,
    pub sync_mode: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanEnvPlan {
    pub vars: Vec<String>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanSecretPlan {
    pub refs: Vec<String>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanRuntimeRefs {
    pub command: Vec<String>,
    pub docs: Vec<String>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanLifecycle {
    pub status: &'static str,
    pub next: Vec<&'static str>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanArtifactPlan {
    pub paths: Vec<String>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanEvidencePlan {
    pub paths: Vec<LabRefreshPlanEvidencePath>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanResultPlan {
    pub run_id: String,
    pub status: &'static str,
    pub refs: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanInspectionPlan {
    pub commands: Vec<LabRefreshPlanCommand>,
    pub unknown: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanCheck {
    pub name: String,
    pub status: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanEvidencePath {
    pub kind: &'static str,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LabRefreshPlanCommand {
    pub label: &'static str,
    pub command: String,
    pub purpose: &'static str,
}

pub fn run(args: LabArgs, _global: &GlobalArgs) -> CmdResult<LabRefreshPlanOutput> {
    match args.command {
        LabCommand::RefreshPlan(args) => refresh_plan(args).map(|output| (output, 0)),
    }
}

fn refresh_plan(args: RefreshPlanArgs) -> homeboy::core::Result<LabRefreshPlanOutput> {
    validate_sync_mode(&args.sync_mode)?;

    if args.command.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "refresh-plan requires a command after --",
            None,
            Some(vec![
                "Example: homeboy lab refresh-plan --runner lab --workspace . --runner-cwd /workspace/app --run-id run-1 --output artifacts/review -- npm test".to_string(),
            ]),
        ));
    }

    let mut checks = Vec::new();
    add_runner_check(&mut checks, &args.runner)?;
    add_path_check(&mut checks, "workspace", &args.workspace)?;
    for source in &args.sources {
        add_path_check(&mut checks, "source", source)?;
    }
    for fixture in &args.fixtures {
        add_path_check(&mut checks, "fixture", fixture)?;
    }

    let evidence_paths = evidence_paths(&args);
    let next_commands = next_commands(&args, &evidence_paths);
    let docs = vec![
        "docs/operators/artifact-loop-runner-matrix.md".to_string(),
        "docs/commands/lab.md".to_string(),
    ];
    let handoff = handoff_plan(&args, &evidence_paths, &next_commands, &docs);

    Ok(LabRefreshPlanOutput {
        variant: "refresh_plan",
        runner: args.runner,
        workspace: args.workspace,
        runner_cwd: args.runner_cwd,
        run_id: args.run_id,
        handoff,
        checks,
        evidence_paths,
        next_commands,
        docs,
    })
}

fn validate_sync_mode(sync_mode: &str) -> homeboy::core::Result<RunnerWorkspaceSyncMode> {
    match sync_mode {
        "snapshot" => Ok(RunnerWorkspaceSyncMode::Snapshot),
        "snapshot-git" => Ok(RunnerWorkspaceSyncMode::SnapshotGit),
        "git" => Ok(RunnerWorkspaceSyncMode::Git),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "sync_mode",
            format!("unsupported sync mode: {sync_mode}"),
            None,
            Some(vec!["Use one of: snapshot, snapshot-git, git".to_string()]),
        )),
    }
}

fn add_runner_check(
    checks: &mut Vec<LabRefreshPlanCheck>,
    runner_id: &str,
) -> homeboy::core::Result<()> {
    let runner = runners::load(runner_id)?;
    let configured_homeboy = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let workspace_root = runner
        .workspace_root
        .as_deref()
        .unwrap_or("runner has no default workspace_root");

    checks.push(LabRefreshPlanCheck {
        name: "runner".to_string(),
        status: "ok",
        detail: format!(
            "configured runner `{runner_id}` uses Homeboy `{configured_homeboy}` with workspace root `{workspace_root}`"
        ),
    });
    checks.push(LabRefreshPlanCheck {
        name: "runner_homeboy_capability".to_string(),
        status: "planned",
        detail: format!(
            "verify with `{}` before dispatching the refresh workload",
            shell_join(&[
                "homeboy",
                "runner",
                "doctor",
                runner_id,
                "--scope",
                "lab-offload",
            ])
        ),
    });

    Ok(())
}

fn add_path_check(
    checks: &mut Vec<LabRefreshPlanCheck>,
    label: &str,
    path: &str,
) -> homeboy::core::Result<()> {
    if !Path::new(path).exists() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            label,
            format!("{label} path does not exist: {path}"),
            None,
            None,
        ));
    }

    checks.push(LabRefreshPlanCheck {
        name: label.to_string(),
        status: "ok",
        detail: path.to_string(),
    });
    Ok(())
}

fn evidence_paths(args: &RefreshPlanArgs) -> Vec<LabRefreshPlanEvidencePath> {
    args.outputs
        .iter()
        .map(|path| LabRefreshPlanEvidencePath {
            kind: "artifact",
            path: path.clone(),
        })
        .chain(
            args.summaries
                .iter()
                .map(|path| LabRefreshPlanEvidencePath {
                    kind: "summary",
                    path: path.clone(),
                }),
        )
        .collect()
}

fn handoff_plan(
    args: &RefreshPlanArgs,
    evidence_paths: &[LabRefreshPlanEvidencePath],
    next_commands: &[LabRefreshPlanCommand],
    docs: &[String],
) -> LabRefreshPlanHandoff {
    let artifact_paths = evidence_paths
        .iter()
        .filter(|path| path.kind == "artifact")
        .map(|path| path.path.clone())
        .collect();
    let inspection_commands = next_commands
        .iter()
        .filter(|command| matches!(command.label, "inspect-artifacts" | "inspect-evidence"))
        .cloned()
        .collect();

    LabRefreshPlanHandoff {
        schema: "homeboy/lab-refresh-handoff/v1",
        run_id: args.run_id.clone(),
        handoff_id: format!("lab-refresh:{}:{}", args.runner, args.run_id),
        workload_id: args.run_id.clone(),
        runner: LabRefreshPlanRunnerHandoff {
            id: args.runner.clone(),
            mode: None,
        },
        workspace: LabRefreshPlanWorkspaceHandoff {
            controller_path: args.workspace.clone(),
            runner_cwd: args.runner_cwd.clone(),
            sync_mode: args.sync_mode.clone(),
        },
        env_plan: LabRefreshPlanEnvPlan {
            vars: Vec::new(),
            unknown: true,
        },
        secret_plan: LabRefreshPlanSecretPlan {
            refs: Vec::new(),
            unknown: true,
        },
        runtime_refs: LabRefreshPlanRuntimeRefs {
            command: args.command.clone(),
            docs: docs.to_vec(),
            unknown: false,
        },
        lifecycle: LabRefreshPlanLifecycle {
            status: "planned",
            next: vec![
                "verify_runner",
                "sync_workspace",
                "run_refresh",
                "inspect_evidence",
            ],
        },
        artifact: LabRefreshPlanArtifactPlan {
            paths: artifact_paths,
            unknown: false,
        },
        evidence: LabRefreshPlanEvidencePlan {
            paths: evidence_paths.to_vec(),
            unknown: false,
        },
        result: LabRefreshPlanResultPlan {
            run_id: args.run_id.clone(),
            status: "planned",
            refs: Vec::new(),
        },
        inspection: LabRefreshPlanInspectionPlan {
            commands: inspection_commands,
            unknown: false,
        },
    }
}

fn next_commands(
    args: &RefreshPlanArgs,
    evidence_paths: &[LabRefreshPlanEvidencePath],
) -> Vec<LabRefreshPlanCommand> {
    let mut runner_exec = vec![
        "homeboy".to_string(),
        "runner".to_string(),
        "exec".to_string(),
        args.runner.clone(),
        "--cwd".to_string(),
        args.runner_cwd.clone(),
        "--run-id".to_string(),
        args.run_id.clone(),
    ];
    for evidence_path in evidence_paths {
        match evidence_path.kind {
            "artifact" => runner_exec.push("--artifact".to_string()),
            "summary" => runner_exec.push("--summary".to_string()),
            _ => continue,
        }
        runner_exec.push(evidence_path.path.clone());
    }
    runner_exec.push("--".to_string());
    runner_exec.extend(args.command.clone());

    vec![
        LabRefreshPlanCommand {
            label: "verify-runner",
            command: shell_join(&[
                "homeboy",
                "runner",
                "doctor",
                &args.runner,
                "--scope",
                "lab-offload",
            ]),
            purpose: "verify runner Homeboy binary, daemon, and Lab offload capability",
        },
        LabRefreshPlanCommand {
            label: "sync-workspace",
            command: shell_join(&[
                "homeboy",
                "runner",
                "workspace",
                "sync",
                &args.runner,
                "--path",
                &args.workspace,
                "--mode",
                &args.sync_mode,
            ]),
            purpose: "materialize the fresh controller workspace on the runner",
        },
        LabRefreshPlanCommand {
            label: "run-refresh",
            command: shell_join_owned(&runner_exec),
            purpose: "execute the workload and declare produced evidence paths",
        },
        LabRefreshPlanCommand {
            label: "inspect-artifacts",
            command: shell_join(&["homeboy", "runs", "artifacts", &args.run_id]),
            purpose: "confirm the produced files are attached to the persisted run",
        },
        LabRefreshPlanCommand {
            label: "inspect-evidence",
            command: shell_join(&["homeboy", "runs", "evidence", &args.run_id]),
            purpose: "get reviewer-facing artifact refs or fetch commands",
        },
    ]
}

fn shell_join(args: &[&str]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_join_owned(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_commands_include_existing_runner_artifact_primitives() {
        let args = RefreshPlanArgs {
            runner: "lab-runner".to_string(),
            workspace: "/workspace/source".to_string(),
            runner_cwd: "/runner/source".to_string(),
            run_id: "matrix-refresh-1".to_string(),
            outputs: vec!["artifacts/matrix".to_string()],
            summaries: vec!["artifacts/matrix/matrix-summary.json".to_string()],
            sources: Vec::new(),
            fixtures: Vec::new(),
            sync_mode: "snapshot".to_string(),
            command: vec![
                "sh".to_string(),
                "-lc".to_string(),
                "./run matrix".to_string(),
            ],
        };
        let evidence = evidence_paths(&args);
        let commands = next_commands(&args, &evidence);

        assert_eq!(commands[0].label, "verify-runner");
        assert_eq!(
            commands[1].command,
            "homeboy runner workspace sync lab-runner --path /workspace/source --mode snapshot"
        );
        assert_eq!(commands[2].label, "run-refresh");
        assert!(commands[2].command.contains(
            "--artifact artifacts/matrix --summary artifacts/matrix/matrix-summary.json"
        ));
        assert!(commands[2].command.contains("-- sh -lc './run matrix'"));
        assert_eq!(
            commands[3].command,
            "homeboy runs artifacts matrix-refresh-1"
        );
        assert_eq!(
            commands[4].command,
            "homeboy runs evidence matrix-refresh-1"
        );
    }

    #[test]
    fn handoff_plan_exposes_typed_generic_fields() {
        let args = RefreshPlanArgs {
            runner: "lab-runner".to_string(),
            workspace: "/workspace/source".to_string(),
            runner_cwd: "/runner/source".to_string(),
            run_id: "matrix-refresh-1".to_string(),
            outputs: vec!["artifacts/matrix".to_string()],
            summaries: vec!["artifacts/matrix/matrix-summary.json".to_string()],
            sources: Vec::new(),
            fixtures: Vec::new(),
            sync_mode: "snapshot-git".to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
        };
        let evidence = evidence_paths(&args);
        let commands = next_commands(&args, &evidence);
        let docs = vec!["docs/commands/lab.md".to_string()];

        let handoff = handoff_plan(&args, &evidence, &commands, &docs);

        assert_eq!(handoff.schema, "homeboy/lab-refresh-handoff/v1");
        assert_eq!(handoff.run_id, "matrix-refresh-1");
        assert_eq!(
            handoff.handoff_id,
            "lab-refresh:lab-runner:matrix-refresh-1"
        );
        assert_eq!(handoff.workload_id, "matrix-refresh-1");
        assert_eq!(handoff.runner.id, "lab-runner");
        assert_eq!(handoff.runner.mode, None);
        assert_eq!(handoff.workspace.controller_path, "/workspace/source");
        assert_eq!(handoff.workspace.runner_cwd, "/runner/source");
        assert_eq!(handoff.workspace.sync_mode, "snapshot-git");
        assert_eq!(handoff.env_plan.vars, Vec::<String>::new());
        assert!(handoff.env_plan.unknown);
        assert_eq!(handoff.secret_plan.refs, Vec::<String>::new());
        assert!(handoff.secret_plan.unknown);
        assert_eq!(handoff.runtime_refs.command, vec!["cargo", "test"]);
        assert_eq!(handoff.artifact.paths, vec!["artifacts/matrix"]);
        assert_eq!(handoff.evidence.paths, evidence);
        assert_eq!(handoff.result.status, "planned");
        assert_eq!(handoff.inspection.commands.len(), 2);
    }

    #[test]
    fn invalid_sync_mode_is_rejected() {
        let args = RefreshPlanArgs {
            runner: "missing-runner".to_string(),
            workspace: "/missing/workspace".to_string(),
            runner_cwd: "/runner/source".to_string(),
            run_id: "matrix-refresh-1".to_string(),
            outputs: Vec::new(),
            summaries: Vec::new(),
            sources: Vec::new(),
            fixtures: Vec::new(),
            sync_mode: "rsync".to_string(),
            command: vec!["cargo".to_string(), "test".to_string()],
        };

        let err = refresh_plan(args).expect_err("sync mode should be validated");

        let message = err.to_string();
        assert!(message.contains("unsupported sync mode: rsync"));
    }

    #[test]
    fn shell_quote_handles_spaces_and_single_quotes() {
        assert_eq!(shell_quote("simple/path"), "simple/path");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's ok"), "'it'\\''s ok'");
    }
}
