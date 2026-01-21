use clap::{Args, ValueEnum};
use serde::Serialize;

use homeboy::component;
use homeboy::deploy::{self, DeployConfig};
use homeboy::release::{self, ReleasePlan, ReleaseRun};

use super::CmdResult;

#[derive(Clone, ValueEnum)]
pub enum BumpType {
    Patch,
    Minor,
    Major,
}

impl BumpType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BumpType::Patch => "patch",
            BumpType::Minor => "minor",
            BumpType::Major => "major",
        }
    }
}

#[derive(Args)]
pub struct ReleaseArgs {
    /// Component ID
    #[arg(value_name = "COMPONENT")]
    component_id: String,

    /// Version bump type (patch, minor, major)
    #[arg(value_name = "BUMP_TYPE")]
    bump_type: BumpType,

    /// Preview what will happen without making changes
    #[arg(long)]
    dry_run: bool,

    /// Custom message for pre-release commit
    #[arg(long, value_name = "MESSAGE")]
    commit_message: Option<String>,

    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    json: bool,

    /// Deploy to all projects using this component after release
    #[arg(long)]
    deploy: bool,
}

#[derive(Serialize)]
pub struct DeploymentResult {
    pub projects: Vec<ProjectDeployResult>,
    pub summary: DeploymentSummary,
}

#[derive(Serialize)]
pub struct ProjectDeployResult {
    pub project_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_result: Option<homeboy::deploy::ComponentDeployResult>,
}

#[derive(Serialize)]
pub struct DeploymentSummary {
    pub total_projects: u32,
    pub succeeded: u32,
    pub failed: u32,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release")]
pub struct ReleaseOutput {
    pub result: ReleaseResult,
}

#[derive(Serialize)]
pub struct ReleaseResult {
    pub component_id: String,
    pub bump_type: String,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<ReleasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<ReleaseRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentResult>,
}

pub fn run(args: ReleaseArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ReleaseOutput> {
    let options = release::ReleaseOptions {
        bump_type: args.bump_type.as_str().to_string(),
        dry_run: args.dry_run,
        commit_message: args.commit_message.clone(),
    };

    if args.dry_run {
        let plan = release::plan(&args.component_id, &options)?;

        let deployment = if args.deploy {
            Some(plan_deployment(&args.component_id))
        } else {
            None
        };

        Ok((
            ReleaseOutput {
                result: ReleaseResult {
                    component_id: args.component_id,
                    bump_type: options.bump_type,
                    dry_run: true,
                    commit_message: args.commit_message,
                    plan: Some(plan),
                    run: None,
                    deployment,
                },
            },
            0,
        ))
    } else {
        let run_result = release::run(&args.component_id, &options)?;

        let (deployment, deploy_exit_code) = if args.deploy {
            execute_deployment(&args.component_id)
        } else {
            (None, 0)
        };

        Ok((
            ReleaseOutput {
                result: ReleaseResult {
                    component_id: args.component_id,
                    bump_type: options.bump_type,
                    dry_run: false,
                    commit_message: args.commit_message,
                    plan: None,
                    run: Some(run_result),
                    deployment,
                },
            },
            deploy_exit_code,
        ))
    }
}

fn plan_deployment(component_id: &str) -> DeploymentResult {
    let projects = component::projects_using(component_id).unwrap_or_default();

    if projects.is_empty() {
        eprintln!(
            "[release] Warning: No projects use component '{}'. Nothing to deploy.",
            component_id
        );
    }

    let project_results: Vec<ProjectDeployResult> = projects
        .iter()
        .map(|project_id| ProjectDeployResult {
            project_id: project_id.clone(),
            status: "planned".to_string(),
            error: None,
            component_result: None,
        })
        .collect();

    let total = project_results.len() as u32;
    DeploymentResult {
        projects: project_results,
        summary: DeploymentSummary {
            total_projects: total,
            succeeded: 0,
            failed: 0,
        },
    }
}

fn execute_deployment(component_id: &str) -> (Option<DeploymentResult>, i32) {
    let projects = component::projects_using(component_id).unwrap_or_default();

    if projects.is_empty() {
        eprintln!(
            "[release] Warning: No projects use component '{}'. Nothing to deploy.",
            component_id
        );
        return (
            Some(DeploymentResult {
                projects: vec![],
                summary: DeploymentSummary {
                    total_projects: 0,
                    succeeded: 0,
                    failed: 0,
                },
            }),
            0,
        );
    }

    eprintln!(
        "[release] Deploying '{}' to {} project(s)...",
        component_id,
        projects.len()
    );

    let mut project_results = Vec::new();
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for project_id in &projects {
        eprintln!("[release] Deploying to project '{}'...", project_id);

        let config = DeployConfig {
            component_ids: vec![component_id.to_string()],
            all: false,
            outdated: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: true,
        };

        match deploy::run(project_id, &config) {
            Ok(result) => {
                let component_result = result.results.into_iter().next();
                let deploy_failed = result.summary.failed > 0;

                if deploy_failed {
                    let error_msg = component_result
                        .as_ref()
                        .and_then(|r| r.error.clone())
                        .unwrap_or_else(|| "Deployment failed".to_string());

                    project_results.push(ProjectDeployResult {
                        project_id: project_id.clone(),
                        status: "failed".to_string(),
                        error: Some(error_msg),
                        component_result,
                    });
                    failed += 1;
                } else {
                    project_results.push(ProjectDeployResult {
                        project_id: project_id.clone(),
                        status: "deployed".to_string(),
                        error: None,
                        component_result,
                    });
                    succeeded += 1;
                }
            }
            Err(e) => {
                project_results.push(ProjectDeployResult {
                    project_id: project_id.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    component_result: None,
                });
                failed += 1;
            }
        }
    }

    let total = project_results.len() as u32;
    let exit_code = if failed > 0 { 1 } else { 0 };

    (
        Some(DeploymentResult {
            projects: project_results,
            summary: DeploymentSummary {
                total_projects: total,
                succeeded,
                failed,
            },
        }),
        exit_code,
    )
}
