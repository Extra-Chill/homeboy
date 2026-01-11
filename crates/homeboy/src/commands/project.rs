use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy_core::config::ConfigManager;

#[derive(Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommand,
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Show project configuration
    Show {
        /// Project ID (uses active project if not specified)
        project_id: Option<String>,
    },
    /// Switch active project
    Switch {
        /// Project ID to switch to
        project_id: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOutput {
    command: String,
    project_id: Option<String>,
    project: Option<homeboy_core::config::ProjectConfiguration>,
}

pub fn run(args: ProjectArgs) -> homeboy_core::Result<(ProjectOutput, i32)> {
    match args.command {
        ProjectCommand::Show { project_id } => show(project_id),
        ProjectCommand::Switch { project_id } => switch(&project_id),
    }
}

fn show(project_id: Option<String>) -> homeboy_core::Result<(ProjectOutput, i32)> {
    let project = match project_id.clone() {
        Some(id) => ConfigManager::load_project(&id)?,
        None => ConfigManager::get_active_project()?,
    };

    Ok((
        ProjectOutput {
            command: "project.show".to_string(),
            project_id: Some(project.id.clone()),
            project: Some(project),
        },
        0,
    ))
}

fn switch(project_id: &str) -> homeboy_core::Result<(ProjectOutput, i32)> {
    ConfigManager::set_active_project(project_id)?;

    let project = ConfigManager::load_project(project_id)?;

    Ok((
        ProjectOutput {
            command: "project.switch".to_string(),
            project_id: Some(project_id.to_string()),
            project: Some(project),
        },
        0,
    ))
}
