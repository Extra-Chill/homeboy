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

    /// Version bump type (patch, minor, major) — not needed with --recover
    #[arg(value_name = "BUMP_TYPE", ignore_case = true, required_unless_present = "recover")]
    bump_type: Option<BumpType>,

    /// Preview what will happen without making changes
    #[arg(long)]
    dry_run: bool,

    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    json: bool,

    /// Deploy to all projects using this component after release
    #[arg(long)]
    deploy: bool,

    /// Recover from an interrupted release (tag + push current version)
    #[arg(long, conflicts_with = "bump_type")]
    recover: bool,
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
    pub plan: Option<ReleasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<ReleaseRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentResult>,
}

pub fn run(args: ReleaseArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ReleaseOutput> {
    if args.recover {
        return run_recover(&args.component_id);
    }

    let bump_type = args.bump_type.expect("bump_type required when not recovering");
    let options = release::ReleaseOptions {
        bump_type: bump_type.as_str().to_string(),
        dry_run: args.dry_run,
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
                    plan: Some(plan),
                    run: None,
                    deployment,
                },
            },
            0,
        ))
    } else {
        let run_result = release::run(&args.component_id, &options)?;
        display_release_summary(&run_result);

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
                    plan: None,
                    run: Some(run_result),
                    deployment,
                },
            },
            deploy_exit_code,
        ))
    }
}

/// Displays release success summary to stderr.
pub fn display_release_summary(run: &ReleaseRun) {
    if let Some(ref summary) = run.result.summary {
        if !summary.success_summary.is_empty() {
            eprintln!();
            for line in &summary.success_summary {
                eprintln!("[release] {}", line);
            }
        }
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

/// Recover from an interrupted release.
/// Detects state: version files bumped but tag/push missing, and completes the release.
fn run_recover(component_id: &str) -> CmdResult<ReleaseOutput> {
    let component = component::load(component_id)?;
    let version_info = homeboy::version::read_version(Some(component_id))?;
    let current_version = &version_info.version;
    let tag_name = format!("v{}", current_version);

    // Check what state we're in
    let tag_exists_local = homeboy::git::tag_exists_locally(&component.local_path, &tag_name)
        .unwrap_or(false);
    let tag_exists_remote = homeboy::git::tag_exists_on_remote(&component.local_path, &tag_name)
        .unwrap_or(false);
    let uncommitted = homeboy::git::get_uncommitted_changes(&component.local_path)?;

    let mut actions = Vec::new();

    // Step 1: Commit uncommitted version files if needed
    if uncommitted.has_changes {
        eprintln!("[recover] Committing uncommitted changes...");
        let msg = format!("release: v{}", current_version);
        let commit_result = homeboy::git::commit(
            Some(component_id),
            Some(msg.as_str()),
            homeboy::git::CommitOptions {
                staged_only: false,
                files: None,
                exclude: None,
                amend: false,
            },
        )?;
        if !commit_result.success {
            return Err(homeboy::Error::other(format!(
                "Failed to commit: {}",
                commit_result.stderr
            )));
        }
        actions.push("committed version files".to_string());
    }

    // Step 2: Create tag if missing locally
    if !tag_exists_local {
        eprintln!("[recover] Creating tag {}...", tag_name);
        let tag_result = homeboy::git::tag(
            Some(component_id),
            Some(&tag_name),
            Some(&format!("Release {}", tag_name)),
        )?;
        if !tag_result.success {
            return Err(homeboy::Error::other(format!(
                "Failed to create tag: {}",
                tag_result.stderr
            )));
        }
        actions.push(format!("created tag {}", tag_name));
    }

    // Step 3: Push commits and tags if not on remote
    if !tag_exists_remote {
        eprintln!("[recover] Pushing to remote...");
        let push_result = homeboy::git::push(Some(component_id), true)?;
        if !push_result.success {
            return Err(homeboy::Error::other(format!(
                "Failed to push: {}",
                push_result.stderr
            )));
        }
        actions.push("pushed commits and tags".to_string());
    }

    if actions.is_empty() {
        eprintln!("[recover] Release v{} appears complete — nothing to recover.", current_version);
    } else {
        eprintln!("[recover] Recovery complete for v{}: {}", current_version, actions.join(", "));
    }

    Ok((
        ReleaseOutput {
            result: ReleaseResult {
                component_id: component_id.to_string(),
                bump_type: "recover".to_string(),
                dry_run: false,
                plan: None,
                run: None,
                deployment: None,
            },
        },
        0,
    ))
}
