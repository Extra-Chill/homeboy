use crate::core::engine::shell;
use crate::core::execution::{execute_plan_steps, ExecutionStatus, ExecutionStepResult};
use crate::core::fleet;
use crate::core::plan::{HomeboyPlan, PlanKind, PlanStep, PlanValues};
use crate::core::project::Project;
use crate::core::server::{resolve_context, SshClient, SshResolveArgs};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Default, Clone, Serialize)]
pub struct FleetExecProjectResult {
    pub project_id: String,
    pub server_id: Option<String>,
    pub base_path: Option<String>,
    pub command: String,
    pub status: String,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct FleetExecSummary {
    pub total: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetExecRun {
    pub plan: HomeboyPlan,
    pub results: Vec<FleetExecProjectResult>,
    pub steps: Vec<ExecutionStepResult>,
    pub summary: FleetExecSummary,
    pub exit_code: i32,
}

pub fn collect_exec(
    fleet_id: &str,
    command: Vec<String>,
    check: bool,
    apply: bool,
    user_override: Option<String>,
) -> crate::core::Result<(Vec<FleetExecProjectResult>, FleetExecSummary, i32)> {
    let run = collect_exec_run(fleet_id, command, check, apply, user_override)?;
    Ok((run.results, run.summary, run.exit_code))
}

pub fn collect_exec_run(
    fleet_id: &str,
    command: Vec<String>,
    check: bool,
    apply: bool,
    user_override: Option<String>,
) -> crate::core::Result<FleetExecRun> {
    if command.is_empty() {
        return Err(
            crate::core::Error::validation_missing_argument(vec!["command".to_string()])
                .with_hint(
                    "Usage: homeboy fleet exec <fleet> --check -- <command> or homeboy fleet exec <fleet> --apply -- <command>"
                        .to_string(),
                ),
        );
    }

    if !check && !apply {
        return Err(crate::core::Error::validation_invalid_argument(
            "apply",
            "fleet exec sends commands over SSH to every project in the fleet and requires explicit --apply. Use --check to preview or re-run with --apply to execute.",
            None,
            Some(vec!["homeboy fleet exec <fleet> --apply -- <command>".to_string()]),
        ));
    }

    let command_string = if command.len() == 1 {
        command[0].clone()
    } else {
        shell::quote_args(&command)
    };

    let resolution = fleet::resolve_projects(fleet_id)?;
    resolution.ensure_complete(fleet_id)?;
    let projects = resolution.projects;

    if projects.is_empty() {
        return Err(crate::core::Error::validation_invalid_argument(
            "fleet",
            "Fleet has no projects",
            Some(fleet_id.to_string()),
            None,
        ));
    }

    let mut summary = FleetExecSummary {
        total: projects.len() as u32,
        ..Default::default()
    };
    let plan = build_exec_plan(fleet_id, &command_string, &projects);

    if check {
        let results = plan
            .steps
            .iter()
            .filter_map(project_result_from_planned_step)
            .collect::<Vec<_>>();
        summary.skipped = summary.total;
        let steps = results
            .iter()
            .map(project_result_to_execution_step)
            .collect::<Vec<_>>();
        return Ok(FleetExecRun {
            plan,
            results,
            steps,
            summary,
            exit_code: 0,
        });
    }

    let projects_by_id = projects
        .iter()
        .map(|project| (project.id.clone(), project))
        .collect::<HashMap<_, _>>();
    let execution = execute_plan_steps(
        &plan.steps,
        |step| {
            let Some(project_id) = step.input_as::<String>("project_id") else {
                return Ok(None);
            };
            let Some(project) = projects_by_id.get(&project_id) else {
                return Ok(Some(FleetExecProjectResult {
                    project_id,
                    command: step
                        .input_as::<String>("command")
                        .unwrap_or_else(|| command_string.clone()),
                    status: "failed".to_string(),
                    error: Some("Planned fleet project is no longer available".to_string()),
                    ..Default::default()
                }));
            };
            Ok(Some(execute_project_step(
                project,
                &command_string,
                user_override.as_deref(),
            )))
        },
        |_| false,
    )?;

    let results = execution.results;
    for result in &results {
        if result.status == "success" {
            summary.succeeded += 1;
        } else {
            summary.failed += 1;
        }
    }

    let exit_code = if summary.failed > 0 { 1 } else { 0 };
    let steps = results
        .iter()
        .map(project_result_to_execution_step)
        .collect::<Vec<_>>();
    Ok(FleetExecRun {
        plan,
        results,
        steps,
        summary,
        exit_code,
    })
}

fn build_exec_plan(fleet_id: &str, command_string: &str, projects: &[Project]) -> HomeboyPlan {
    let steps = projects
        .iter()
        .map(|project| {
            PlanStep::ready(format!("fleet.exec.{}", project.id), "fleet_exec_project")
                .label(format!("Execute command on {}", project.id))
                .inputs(
                    PlanValues::new()
                        .string("fleet_id", fleet_id)
                        .string("project_id", &project.id)
                        .string("command", planned_command(project, command_string))
                        .json("server_id", &project.server_id)
                        .json("base_path", &project.base_path),
                )
                .build()
        })
        .collect::<Vec<_>>();

    HomeboyPlan::builder_for_description(PlanKind::Fleet, format!("fleet exec {fleet_id}"))
        .inputs(
            PlanValues::new()
                .string("fleet_id", fleet_id)
                .string("command", command_string),
        )
        .steps(steps)
        .summarize()
        .build()
}

fn project_result_from_planned_step(step: &PlanStep) -> Option<FleetExecProjectResult> {
    Some(FleetExecProjectResult {
        project_id: step.input_as::<String>("project_id")?,
        server_id: step.input_as::<Option<String>>("server_id").flatten(),
        base_path: step.input_as::<Option<String>>("base_path").flatten(),
        command: step.input_as::<String>("command")?,
        status: "planned".to_string(),
        ..Default::default()
    })
}

fn execute_project_step(
    project: &Project,
    command_string: &str,
    user_override: Option<&str>,
) -> FleetExecProjectResult {
    let server_id = project.server_id.clone();

    let resolve_result = match resolve_context(&SshResolveArgs {
        id: None,
        project: Some(project.id.clone()),
        server: None,
    }) {
        Ok(r) => r,
        Err(e) => return failed_project_result(project, &server_id, command_string, &e),
    };

    let mut client = match SshClient::from_server(&resolve_result.server, &resolve_result.server_id)
    {
        Ok(c) => c,
        Err(e) => return failed_project_result(project, &server_id, command_string, &e),
    };

    if let Some(user) = user_override {
        client.user = user.to_string();
    }

    let effective_cmd = match &resolve_result.base_path {
        Some(bp) => format!("cd {} && {}", shell::quote_path(bp), command_string),
        None => command_string.to_string(),
    };

    let output = client.execute(&effective_cmd);

    FleetExecProjectResult {
        project_id: project.id.clone(),
        server_id,
        base_path: project.base_path.clone(),
        command: effective_cmd,
        status: if output.success {
            "success".to_string()
        } else {
            "failed".to_string()
        },
        stdout: Some(output.stdout),
        stderr: Some(output.stderr),
        exit_code: Some(output.exit_code),
        error: None,
    }
}

fn project_result_to_execution_step(result: &FleetExecProjectResult) -> ExecutionStepResult {
    let status = match result.status.as_str() {
        "success" => ExecutionStatus::Success,
        "planned" => ExecutionStatus::Skipped,
        "failed" => ExecutionStatus::Failed,
        _ => ExecutionStatus::PartialSuccess,
    };

    ExecutionStepResult {
        id: format!("fleet.exec.{}", result.project_id),
        kind: "fleet_exec_project".to_string(),
        status,
        summary: Some(format!("{}: {}", result.project_id, result.status)),
        artifacts: Vec::new(),
        warnings: Vec::new(),
        data: Some(serde_json::json!({
            "project_id": result.project_id,
            "server_id": result.server_id,
            "base_path": result.base_path,
            "command": result.command,
            "exit_code": result.exit_code,
        })),
        error: result.error.clone(),
    }
}

fn planned_command(project: &Project, command_string: &str) -> String {
    match &project.base_path {
        Some(bp) => format!("cd {} && {}", shell::quote_path(bp), command_string),
        None => command_string.to_string(),
    }
}

fn failed_project_result(
    project: &Project,
    server_id: &Option<String>,
    command: &str,
    error: &crate::core::Error,
) -> FleetExecProjectResult {
    FleetExecProjectResult {
        project_id: project.id.clone(),
        server_id: server_id.clone(),
        base_path: project.base_path.clone(),
        command: command.to_string(),
        status: "failed".to_string(),
        error: Some(error.to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plan::PlanStepStatus;

    #[test]
    fn builds_fleet_exec_plan_from_projects() {
        let projects = vec![Project {
            id: "alpha".to_string(),
            server_id: Some("server-a".to_string()),
            base_path: Some("/srv/alpha".to_string()),
            ..Default::default()
        }];

        let plan = build_exec_plan("production", "wp option get home", &projects);

        assert_eq!(PlanKind::Fleet, plan.kind);
        assert_eq!(1, plan.steps.len());
        assert_eq!("fleet.exec.alpha", plan.steps[0].id);
        assert_eq!(PlanStepStatus::Ready, plan.steps[0].status);
        assert_eq!(
            Some("cd '/srv/alpha' && wp option get home".to_string()),
            plan.steps[0].input_as::<String>("command")
        );
    }

    #[test]
    fn maps_planned_project_result_to_execution_step() {
        let result = FleetExecProjectResult {
            project_id: "alpha".to_string(),
            command: "wp option get home".to_string(),
            status: "planned".to_string(),
            ..Default::default()
        };

        let step = project_result_to_execution_step(&result);

        assert_eq!("fleet.exec.alpha", step.id);
        assert_eq!(ExecutionStatus::Skipped, step.status);
    }

    #[test]
    fn real_fleet_exec_requires_apply_before_loading_fleet() {
        let error = collect_exec_run(
            "missing-fleet",
            vec!["wp".to_string(), "plugin".to_string(), "list".to_string()],
            false,
            false,
            None,
        )
        .expect_err("real fleet exec should require --apply");

        assert!(error.message.contains("requires explicit --apply"));
    }

    #[test]
    fn fleet_exec_fails_when_fleet_references_missing_project() {
        crate::test_support::with_isolated_home(|_| {
            fleet::save(&fleet::Fleet::new(
                "production".to_string(),
                vec!["missing-site".to_string()],
            ))
            .expect("fleet config");

            let error = collect_exec_run(
                "production",
                vec!["wp".to_string(), "plugin".to_string(), "list".to_string()],
                true,
                false,
                None,
            )
            .expect_err("unresolved fleet projects should block exec planning");

            assert!(error.message.contains("references unresolved project"));
            assert!(error.message.contains("missing-site"));
        });
    }
}
