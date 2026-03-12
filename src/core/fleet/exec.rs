use crate::engine::shell;
use crate::fleet;
use crate::project::Project;
use crate::ssh::{resolve_context, SshClient, SshResolveArgs};
use serde::Serialize;

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

pub fn collect_exec(
    fleet_id: &str,
    command: Vec<String>,
    check: bool,
) -> crate::Result<(Vec<FleetExecProjectResult>, FleetExecSummary, i32)> {
    if command.is_empty() {
        return Err(
            crate::Error::validation_missing_argument(vec!["command".to_string()])
                .with_hint("Usage: homeboy fleet exec <fleet> -- <command>".to_string()),
        );
    }

    let command_string = if command.len() == 1 {
        command[0].clone()
    } else {
        shell::quote_args(&command)
    };

    let projects = fleet::get_projects(fleet_id)?;

    if projects.is_empty() {
        return Err(crate::Error::validation_invalid_argument(
            "fleet",
            "Fleet has no projects",
            Some(fleet_id.to_string()),
            None,
        ));
    }

    let mut results: Vec<FleetExecProjectResult> = Vec::new();
    let mut summary = FleetExecSummary {
        total: projects.len() as u32,
        ..Default::default()
    };

    for proj in &projects {
        let server_id = proj.server_id.clone();

        if check {
            let effective_cmd = planned_command(proj, &command_string);
            results.push(FleetExecProjectResult {
                project_id: proj.id.clone(),
                server_id: server_id.clone(),
                base_path: proj.base_path.clone(),
                command: effective_cmd,
                status: "planned".to_string(),
                ..Default::default()
            });
            continue;
        }

        let resolve_result = match resolve_context(&SshResolveArgs {
            id: None,
            project: Some(proj.id.clone()),
            server: None,
        }) {
            Ok(r) => r,
            Err(e) => {
                summary.failed += 1;
                results.push(FleetExecProjectResult {
                    project_id: proj.id.clone(),
                    server_id: server_id.clone(),
                    base_path: proj.base_path.clone(),
                    command: command_string.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    ..Default::default()
                });
                continue;
            }
        };

        let client = match SshClient::from_server(&resolve_result.server, &resolve_result.server_id)
        {
            Ok(c) => c,
            Err(e) => {
                summary.failed += 1;
                results.push(FleetExecProjectResult {
                    project_id: proj.id.clone(),
                    server_id: server_id.clone(),
                    base_path: proj.base_path.clone(),
                    command: command_string.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    ..Default::default()
                });
                continue;
            }
        };

        let effective_cmd = match &resolve_result.base_path {
            Some(bp) => format!("cd {} && {}", shell::quote_path(bp), &command_string),
            None => command_string.clone(),
        };

        let output = client.execute(&effective_cmd);

        if output.success {
            summary.succeeded += 1;
        } else {
            summary.failed += 1;
        }

        results.push(FleetExecProjectResult {
            project_id: proj.id.clone(),
            server_id: server_id.clone(),
            base_path: proj.base_path.clone(),
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
        });
    }

    if check {
        summary.skipped = summary.total;
    }

    let exit_code = if summary.failed > 0 { 1 } else { 0 };
    Ok((results, summary, exit_code))
}

fn planned_command(project: &Project, command_string: &str) -> String {
    match &project.base_path {
        Some(bp) => format!("cd {} && {}", shell::quote_path(bp), command_string),
        None => command_string.to_string(),
    }
}
