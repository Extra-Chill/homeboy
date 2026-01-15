use clap::Args;
use serde::Serialize;

use homeboy::component::{self, Component};
use homeboy::context::{self, ContextOutput};
use homeboy::module::{is_module_compatible, is_module_linked, is_module_ready, load_all_modules};
use homeboy::project::{self, Project};
use homeboy::server::{self, Server};

use super::CmdResult;

#[derive(Args)]
pub struct InitArgs {}

#[derive(Debug, Serialize)]
pub struct InitOutput {
    pub command: &'static str,
    pub context: ContextOutput,
    pub servers: Vec<Server>,
    pub projects: Vec<ProjectListItem>,
    pub components: Vec<Component>,
    pub modules: Vec<ModuleEntry>,
}

#[derive(Debug, Serialize)]
pub struct ProjectListItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

impl From<Project> for ProjectListItem {
    fn from(p: Project) -> Self {
        Self {
            id: p.id,
            domain: p.domain,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ModuleEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub compatible: bool,
    pub ready: bool,
    pub linked: bool,
}

pub fn run_json(_args: InitArgs) -> CmdResult<InitOutput> {
    // Get context for current directory
    let (context_output, _) = context::run(None)?;

    // Get all servers
    let servers = server::list().unwrap_or_default();

    // Get all projects
    let projects: Vec<ProjectListItem> = project::list()
        .unwrap_or_default()
        .into_iter()
        .map(ProjectListItem::from)
        .collect();

    // Get all components
    let components = component::list().unwrap_or_default();

    // Get all modules with status info
    let all_modules = load_all_modules();
    let modules: Vec<ModuleEntry> = all_modules
        .iter()
        .map(|m| ModuleEntry {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            description: m
                .description
                .as_ref()
                .and_then(|d| d.lines().next())
                .unwrap_or("")
                .to_string(),
            runtime: if m.runtime.is_some() {
                "executable"
            } else {
                "platform"
            }
            .to_string(),
            compatible: is_module_compatible(m, None),
            ready: is_module_ready(m),
            linked: is_module_linked(&m.id),
        })
        .collect();

    Ok((
        InitOutput {
            command: "init",
            context: context_output,
            servers,
            projects,
            components,
            modules,
        },
        0,
    ))
}
