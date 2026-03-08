use clap::Args;
use homeboy::log_status;
use serde::Serialize;

use homeboy::component;
use homeboy::deploy::{self, DeployConfig};
use homeboy::git;
use homeboy::release::{self, ReleasePlan, ReleaseRun};

use super::args::{DryRunArgs, HiddenJsonArgs, PositionalComponentArgs};
use super::{CmdResult, ProjectsSummary};

#[derive(Args)]
pub struct ReleaseArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    #[command(flatten)]
    dry_run_args: DryRunArgs,

    #[command(flatten)]
    _json: HiddenJsonArgs,

    /// Deploy to all projects using this component after release
    #[arg(long)]
    deploy: bool,

    /// Recover from an interrupted release (tag + push current version)
    #[arg(long)]
    recover: bool,

    /// Skip pre-release lint and test checks
    #[arg(long)]
    skip_checks: bool,

    /// Allow a major version bump. Required when commits contain breaking changes.
    /// Without this flag, homeboy will warn and exit instead of releasing a major bump.
    #[arg(long)]
    major: bool,

    /// Skip publish/package steps (version bump + tag + push only).
    /// Use when CI handles publishing after the tag is pushed.
    #[arg(long)]
    skip_publish: bool,
}

#[derive(Serialize)]
pub struct DeploymentResult {
    pub projects: Vec<ProjectDeployResult>,
    pub summary: ProjectsSummary,
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
#[serde(tag = "command", rename = "release")]
pub struct ReleaseOutput {
    pub result: ReleaseResult,
}

#[derive(Serialize)]
pub struct ReleaseResult {
    pub component_id: String,
    pub bump_type: String,
    pub dry_run: bool,
    /// Number of releasable commits that drove the bump decision
    pub releasable_commits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<ReleasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<ReleaseRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<DeploymentResult>,
}

pub fn run(args: ReleaseArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ReleaseOutput> {
    let component_id = args.comp.id().to_string();

    if args.recover {
        return run_recover(&args.comp);
    }

    // Resolve bump type from conventional commits
    let component = args.comp.load()?;
    let (bump_type, releasable_count) = match resolve_bump(&component.local_path)? {
        Some(result) => result,
        None => {
            log_status!(
                "release",
                "No releasable commits since last tag — nothing to release"
            );
            return Ok((
                ReleaseOutput {
                    result: ReleaseResult {
                        component_id,
                        bump_type: "none".to_string(),
                        dry_run: args.dry_run_args.dry_run,
                        releasable_commits: 0,
                        new_version: None,
                        tag: None,
                        skipped_reason: Some("no-releasable-commits".to_string()),
                        plan: None,
                        run: None,
                        deployment: None,
                    },
                },
                0,
            ));
        }
    };

    // Safety gate: major bumps require --major flag
    if bump_type == "major" && !args.major {
        log_status!(
            "release",
            "Commits require a major version bump (breaking changes detected)"
        );
        log_status!(
            "release",
            "Re-run with --major to confirm: homeboy release {} --major",
            component_id
        );
        return Ok((
            ReleaseOutput {
                result: ReleaseResult {
                    component_id,
                    bump_type: "major".to_string(),
                    dry_run: args.dry_run_args.dry_run,
                    releasable_commits: releasable_count,
                    new_version: None,
                    tag: None,
                    skipped_reason: Some("major-requires-flag".to_string()),
                    plan: None,
                    run: None,
                    deployment: None,
                },
            },
            0,
        ));
    }

    log_status!(
        "release",
        "Detected {} bump from {} releasable commit{}",
        bump_type,
        releasable_count,
        if releasable_count == 1 { "" } else { "s" }
    );

    let options = release::ReleaseOptions {
        bump_type: bump_type.clone(),
        dry_run: args.dry_run_args.dry_run,
        path_override: args.comp.path.clone(),
        skip_checks: args.skip_checks,
        skip_publish: args.skip_publish,
    };

    if args.dry_run_args.dry_run {
        let plan = release::plan(&component_id, &options)?;

        // Extract new version from plan steps
        let new_version = extract_new_version_from_plan(&plan);
        let tag = new_version.as_ref().map(|v| format!("v{}", v));

        let deployment = if args.deploy {
            Some(plan_deployment(&component_id))
        } else {
            None
        };

        Ok((
            ReleaseOutput {
                result: ReleaseResult {
                    component_id,
                    bump_type,
                    dry_run: true,
                    releasable_commits: releasable_count,
                    new_version,
                    tag,
                    skipped_reason: None,
                    plan: Some(plan),
                    run: None,
                    deployment,
                },
            },
            0,
        ))
    } else {
        let run_result = release::run(&component_id, &options)?;
        display_release_summary(&run_result);

        // Extract version from run result steps
        let new_version = extract_new_version_from_run(&run_result);
        let tag = new_version.as_ref().map(|v| format!("v{}", v));

        // Exit code 3 when release succeeded but post-release hooks failed.
        // Distinct from 1 (release failure) so callers can distinguish.
        let post_release_exit = if has_post_release_warnings(&run_result) {
            3
        } else {
            0
        };

        let (deployment, deploy_exit_code) = if args.deploy {
            execute_deployment(&component_id)
        } else {
            (None, 0)
        };

        // deploy failure (1) takes priority over post-release warning (3)
        let exit_code = if deploy_exit_code != 0 {
            deploy_exit_code
        } else {
            post_release_exit
        };

        Ok((
            ReleaseOutput {
                result: ReleaseResult {
                    component_id,
                    bump_type,
                    dry_run: false,
                    releasable_commits: releasable_count,
                    new_version,
                    tag,
                    skipped_reason: None,
                    plan: None,
                    run: Some(run_result),
                    deployment,
                },
            },
            exit_code,
        ))
    }
}

/// Resolve the bump type from conventional commits since the last tag.
///
/// Returns `Some((bump_type, releasable_count))` if there are releasable commits,
/// or `None` if all commits are docs/chore/merge (nothing to release).
fn resolve_bump(local_path: &str) -> homeboy::error::Result<Option<(String, usize)>> {
    let latest_tag = git::get_latest_tag(local_path)?;
    let commits = git::get_commits_since_tag(local_path, latest_tag.as_deref())?;

    if commits.is_empty() {
        return Ok(None);
    }

    let recommended = git::recommended_bump_from_commits(&commits);

    match recommended {
        Some(bump) => {
            let releasable = commits
                .iter()
                .filter(|c| c.category.to_changelog_entry_type().is_some())
                .count();
            Ok(Some((bump.as_str().to_string(), releasable)))
        }
        None => Ok(None),
    }
}

/// Extract new version from a release plan's version step config.
fn extract_new_version_from_plan(plan: &ReleasePlan) -> Option<String> {
    plan.steps
        .iter()
        .find(|s| s.step_type == "version")
        .and_then(|s| s.config.get("to"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Extract new version from a release run's version step data.
fn extract_new_version_from_run(run: &ReleaseRun) -> Option<String> {
    run.result
        .steps
        .iter()
        .find(|s| s.step_type == "version")
        .and_then(|s| s.data.as_ref())
        .and_then(|d| d.get("new_version").or_else(|| d.get("to")))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Displays release success summary to stderr.
pub fn display_release_summary(run: &ReleaseRun) {
    if let Some(ref summary) = run.result.summary {
        if !summary.success_summary.is_empty() {
            eprintln!();
            for line in &summary.success_summary {
                log_status!("release", "{}", line);
            }
        }
    }
}

/// Returns true if any post-release step had hook failures.
/// Checks the structured `all_succeeded` field in the step data.
pub fn has_post_release_warnings(run: &ReleaseRun) -> bool {
    run.result.steps.iter().any(|step| {
        step.step_type == "post_release"
            && step
                .data
                .as_ref()
                .and_then(|d| d.get("all_succeeded"))
                .and_then(|v| v.as_bool())
                == Some(false)
    })
}

fn plan_deployment(component_id: &str) -> DeploymentResult {
    let projects = component::projects_using(component_id).unwrap_or_default();

    if projects.is_empty() {
        log_status!(
            "release",
            "Warning: No projects use component '{}'. Nothing to deploy.",
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
        summary: ProjectsSummary {
            total_projects: total,
            succeeded: 0,
            failed: 0,
            skipped: 0,
            planned: 0,
        },
    }
}

fn execute_deployment(component_id: &str) -> (Option<DeploymentResult>, i32) {
    let projects = component::projects_using(component_id).unwrap_or_default();

    if projects.is_empty() {
        log_status!(
            "release",
            "Warning: No projects use component '{}'. Nothing to deploy.",
            component_id
        );
        return (
            Some(DeploymentResult {
                projects: vec![],
                summary: ProjectsSummary {
                    total_projects: 0,
                    succeeded: 0,
                    failed: 0,
                    skipped: 0,
                    planned: 0,
                },
            }),
            0,
        );
    }

    log_status!(
        "release",
        "Deploying '{}' to {} project(s)...",
        component_id,
        projects.len()
    );

    let mut project_results = Vec::new();
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for project_id in &projects {
        log_status!("release", "Deploying to project '{}'...", project_id);

        let config = DeployConfig {
            component_ids: vec![component_id.to_string()],
            all: false,
            outdated: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: true,
            keep_deps: false,
            expected_version: None, // Release already validated version
            no_pull: true,          // Release already pushed, no need to pull
            head: true,             // Release just tagged — deploy from current state
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
            summary: ProjectsSummary {
                total_projects: total,
                succeeded,
                failed,
                skipped: 0,
                planned: 0,
            },
        }),
        exit_code,
    )
}

/// Recover from an interrupted release.
/// Detects state: version files bumped but tag/push missing, and completes the release.
fn run_recover(comp_args: &PositionalComponentArgs) -> CmdResult<ReleaseOutput> {
    let component = comp_args.load()?;
    let component_id = comp_args.id();
    let version_info = homeboy::version::read_version(Some(component_id))?;
    let current_version = &version_info.version;
    let tag_name = format!("v{}", current_version);

    // Check what state we're in
    let tag_exists_local =
        homeboy::git::tag_exists_locally(&component.local_path, &tag_name).unwrap_or(false);
    let tag_exists_remote =
        homeboy::git::tag_exists_on_remote(&component.local_path, &tag_name).unwrap_or(false);
    let uncommitted = homeboy::git::get_uncommitted_changes(&component.local_path)?;

    let mut actions = Vec::new();

    // Step 1: Commit uncommitted version files if needed
    if uncommitted.has_changes {
        log_status!("recover", "Committing uncommitted changes...");
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
            return Err(homeboy::Error::git_command_failed(format!(
                "Failed to commit: {}",
                commit_result.stderr
            )));
        }
        actions.push("committed version files".to_string());
    }

    // Step 2: Create tag if missing locally
    if !tag_exists_local {
        log_status!("recover", "Creating tag {}...", tag_name);
        let tag_result = homeboy::git::tag(
            Some(component_id),
            Some(&tag_name),
            Some(&format!("Release {}", tag_name)),
        )?;
        if !tag_result.success {
            return Err(homeboy::Error::git_command_failed(format!(
                "Failed to create tag: {}",
                tag_result.stderr
            )));
        }
        actions.push(format!("created tag {}", tag_name));
    }

    // Step 3: Push commits and tags if not on remote
    if !tag_exists_remote {
        log_status!("recover", "Pushing to remote...");
        let push_result = homeboy::git::push(Some(component_id), true)?;
        if !push_result.success {
            return Err(homeboy::Error::git_command_failed(format!(
                "Failed to push: {}",
                push_result.stderr
            )));
        }
        actions.push("pushed commits and tags".to_string());
    }

    if actions.is_empty() {
        log_status!(
            "recover",
            "Release v{} appears complete — nothing to recover.",
            current_version
        );
    } else {
        log_status!(
            "recover",
            "Recovery complete for v{}: {}",
            current_version,
            actions.join(", ")
        );
    }

    Ok((
        ReleaseOutput {
            result: ReleaseResult {
                component_id: component_id.to_string(),
                bump_type: "recover".to_string(),
                dry_run: false,
                releasable_commits: 0,
                new_version: None,
                tag: None,
                skipped_reason: None,
                plan: None,
                run: None,
                deployment: None,
            },
        },
        0,
    ))
}
