use clap::Args;
use serde::Serialize;

use homeboy_core::config::ConfigManager;

#[derive(Args)]
pub struct ProjectsArgs {
    /// Show only the active project ID
    #[arg(long)]
    current: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsOutput {
    command: String,
    active_project_id: Option<String>,
    projects: Option<Vec<ProjectListItem>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListItem {
    id: String,
    name: String,
    domain: String,
    project_type: String,
    active: bool,
}

pub fn run(args: ProjectsArgs) -> homeboy_core::Result<(ProjectsOutput, i32)> {
    let app_config = ConfigManager::load_app_config()?;
    let active_id = app_config.active_project_id.clone();

    if args.current {
        return Ok((
            ProjectsOutput {
                command: "projects.current".to_string(),
                active_project_id: active_id,
                projects: None,
            },
            0,
        ));
    }

    let projects = ConfigManager::list_projects()?;

    let items: Vec<ProjectListItem> = projects
        .into_iter()
        .map(|project| ProjectListItem {
            active: active_id.as_ref().is_some_and(|a| a == &project.id),
            id: project.id,
            name: project.name,
            domain: project.domain,
            project_type: project.project_type,
        })
        .collect();

    Ok((
        ProjectsOutput {
            command: "projects.list".to_string(),
            active_project_id: active_id,
            projects: Some(items),
        },
        0,
    ))
}
