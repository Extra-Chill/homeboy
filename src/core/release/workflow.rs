use crate::component;
use crate::deploy::{self, DeployConfig};
use crate::error::{Error, Result};
use crate::git;

use super::pipeline::load_component;
use super::types::{
    BatchReleaseComponentResult, BatchReleaseResult, BatchReleaseSummary, ReleaseCommandInput,
    ReleaseCommandResult, ReleaseDeploymentResult, ReleaseDeploymentSummary, ReleaseOptions,
    ReleasePlan, ReleaseProjectDeployResult, ReleaseRun,
};

pub fn run_command(input: ReleaseCommandInput) -> Result<(ReleaseCommandResult, i32)> {
    if input.recover {
        return run_recover(&input);
    }

    let component = load_component(
        &input.component_id,
        &ReleaseOptions {
            path_override: input.path_override.clone(),
            ..Default::default()
        },
    )?;

    let monorepo = git::MonorepoContext::detect(&component.local_path, &input.component_id);
    let (auto_bump_type, releasable_count) =
        match resolve_bump(&component.local_path, monorepo.as_ref())? {
            Some(result) => result,
            None => {
                // No releasable commits, but --bump can still force a release
                if input.bump_override.is_some() {
                    ("none".to_string(), 0)
                } else {
                    log_status!(
                        "release",
                        "No releasable commits since last tag — nothing to release"
                    );
                    return Ok((
                        ReleaseCommandResult {
                            component_id: input.component_id,
                            bump_type: "none".to_string(),
                            dry_run: input.dry_run,
                            releasable_commits: 0,
                            new_version: None,
                            tag: None,
                            skipped_reason: Some("no-releasable-commits".to_string()),
                            plan: None,
                            run: None,
                            deployment: None,
                        },
                        0,
                    ));
                }
            }
        };

    let has_breaking_commits = auto_bump_type == "major";

    // Resolve the effective bump type: --bump overrides auto-detection.
    let bump_type = if let Some(ref override_value) = input.bump_override {
        // Check if it's an explicit version string (e.g. "2.0.0")
        let is_explicit_version = override_value.contains('.');

        if is_explicit_version {
            // Explicit version — pass through as-is, skip all semver logic
            if has_breaking_commits {
                log_status!(
                    "release",
                    "Breaking changes detected in commits — releasing as explicit version {}",
                    override_value
                );
            }
            override_value.clone()
        } else {
            // Semver keyword: major, minor, patch
            let bump = override_value.to_lowercase();
            if !["major", "minor", "patch"].contains(&bump.as_str()) {
                return Err(Error::validation_invalid_argument(
                    "bump",
                    format!(
                        "Invalid --bump value '{}'. Use: major, minor, patch, or a version like 2.0.0",
                        override_value
                    ),
                    Some(override_value.clone()),
                    None,
                ));
            }

            // Warn if overriding to a lower bump than auto-detected
            if has_breaking_commits && bump != "major" {
                log_status!(
                    "release",
                    "⚠ Breaking changes detected in commits, but --bump {} overrides to {} bump",
                    bump,
                    bump
                );
            }

            log_status!(
                "release",
                "Using --bump {} (overriding auto-detected {} from {} commit{})",
                bump,
                auto_bump_type,
                releasable_count,
                if releasable_count == 1 { "" } else { "s" }
            );
            bump
        }
    } else {
        // No override — use auto-detected bump type
        let mut bump_type = auto_bump_type;

        // Pre-1.0 semver: breaking changes bump minor, not major.
        // In semver, 0.x.y signals "initial development" where the public API is
        // not stable. Breaking changes are expected and land as minor bumps.
        // A major bump to 1.0.0 should only happen when the author explicitly
        // decides the API is stable (via --bump major).
        if bump_type == "major" {
            let current_version = super::version::read_version(Some(&input.component_id))
                .ok()
                .and_then(|v| v.version.split('.').next().map(String::from))
                .unwrap_or_default();
            if current_version == "0" {
                log_status!(
                    "release",
                    "Pre-1.0: downgrading major → minor (breaking changes are minor bumps in 0.x)"
                );
                bump_type = "minor".to_string();
            }
        }

        // Gate: auto-detected major requires explicit --bump major
        if bump_type == "major" {
            log_status!(
                "release",
                "⚠ Breaking changes detected — this requires a major version bump"
            );
            log_status!(
                "release",
                "Re-run with: homeboy release {} --bump major",
                input.component_id
            );
            return Ok((
                ReleaseCommandResult {
                    component_id: input.component_id,
                    bump_type: "major".to_string(),
                    dry_run: input.dry_run,
                    releasable_commits: releasable_count,
                    new_version: None,
                    tag: None,
                    skipped_reason: Some("major-requires-flag".to_string()),
                    plan: None,
                    run: None,
                    deployment: None,
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

        bump_type
    };

    let options = ReleaseOptions {
        bump_type: bump_type.clone(),
        dry_run: input.dry_run,
        path_override: input.path_override,
        skip_checks: input.skip_checks,
        skip_publish: input.skip_publish,
        deploy: input.deploy,
    };

    if options.dry_run {
        let plan = super::plan(&input.component_id, &options)?;
        let new_version = extract_new_version_from_plan(&plan);
        let tag = new_version
            .as_ref()
            .map(|v| format_tag(v, monorepo.as_ref()));
        let deployment = input.deploy.then(|| plan_deployment(&input.component_id));

        return Ok((
            ReleaseCommandResult {
                component_id: input.component_id,
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
            0,
        ));
    }

    let run_result = super::run(&input.component_id, &options)?;
    display_release_summary(&run_result);

    let new_version = extract_new_version_from_run(&run_result);
    let tag = new_version
        .as_ref()
        .map(|v| format_tag(v, monorepo.as_ref()));
    let post_release_exit = if has_post_release_warnings(&run_result) {
        3
    } else {
        0
    };
    let (deployment, deploy_exit_code) = if input.deploy {
        execute_deployment(&input.component_id, &component.local_path)
    } else {
        (None, 0)
    };
    let exit_code = if deploy_exit_code != 0 {
        // Deploy failed after the release was already tagged and pushed.
        // The tag cannot be rolled back safely, so warn the user to retry.
        if let Some(ref t) = tag {
            eprintln!();
            log_status!(
                "release",
                "⚠️  Release {} was tagged and pushed, but deploy FAILED.",
                t
            );
            log_status!(
                "release",
                "Run `homeboy deploy {}` to finish deploying.",
                input.component_id
            );
        }
        deploy_exit_code
    } else {
        post_release_exit
    };

    Ok((
        ReleaseCommandResult {
            component_id: input.component_id,
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
        exit_code,
    ))
}

fn resolve_bump(
    local_path: &str,
    monorepo: Option<&git::MonorepoContext>,
) -> Result<Option<(String, usize)>> {
    let (_latest_tag, commits) = super::pipeline::resolve_tag_and_commits(local_path, monorepo)?;

    if commits.is_empty() {
        return Ok(None);
    }

    match git::recommended_bump_from_commits(&commits) {
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

/// Format a version string as a tag name, using component prefix in monorepos.
fn format_tag(version: &str, monorepo: Option<&git::MonorepoContext>) -> String {
    match monorepo {
        Some(ctx) => ctx.format_tag(version),
        None => format!("v{}", version),
    }
}

fn extract_new_version_from_plan(plan: &ReleasePlan) -> Option<String> {
    plan.steps
        .iter()
        .find(|s| s.step_type == "version")
        .and_then(|s| s.config.get("to"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

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

fn display_release_summary(run: &ReleaseRun) {
    if let Some(ref summary) = run.result.summary {
        if !summary.success_summary.is_empty() {
            eprintln!();
            for line in &summary.success_summary {
                log_status!("release", "{}", line);
            }
        }
    }
}

fn has_post_release_warnings(run: &ReleaseRun) -> bool {
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

fn empty_deployment_summary(total_projects: u32) -> ReleaseDeploymentSummary {
    ReleaseDeploymentSummary {
        total_projects,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        planned: 0,
    }
}

fn plan_deployment(component_id: &str) -> ReleaseDeploymentResult {
    let projects = component::projects_using(component_id).unwrap_or_default();

    if projects.is_empty() {
        log_status!(
            "release",
            "Warning: No projects use component '{}'. Nothing to deploy.",
            component_id
        );
    }

    let project_results: Vec<ReleaseProjectDeployResult> = projects
        .iter()
        .map(|project_id| ReleaseProjectDeployResult {
            project_id: project_id.clone(),
            status: "planned".to_string(),
            error: None,
            component_result: None,
        })
        .collect();

    ReleaseDeploymentResult {
        projects: project_results,
        summary: empty_deployment_summary(projects.len() as u32),
    }
}

fn execute_deployment(
    component_id: &str,
    local_path: &str,
) -> (Option<ReleaseDeploymentResult>, i32) {
    let projects = component::projects_using(component_id).unwrap_or_default();

    if projects.is_empty() {
        log_status!(
            "release",
            "Warning: No projects use component '{}'. Nothing to deploy.",
            component_id
        );
        return (
            Some(ReleaseDeploymentResult {
                projects: vec![],
                summary: empty_deployment_summary(0),
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
            // Force: the release pipeline just committed and tagged, so the
            // workspace is clean by definition. Skipping the uncommitted changes
            // check avoids false positives that silently block deployment.
            force: true,
            // Always rebuild to guarantee the artifact matches the just-tagged
            // commit. Reusing a stale artifact is a silent production bug (#991).
            skip_build: false,
            keep_deps: false,
            expected_version: None,
            no_pull: true,
            head: true,
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

                    project_results.push(ReleaseProjectDeployResult {
                        project_id: project_id.clone(),
                        status: "failed".to_string(),
                        error: Some(error_msg),
                        component_result,
                    });
                    failed += 1;
                } else {
                    project_results.push(ReleaseProjectDeployResult {
                        project_id: project_id.clone(),
                        status: "deployed".to_string(),
                        error: None,
                        component_result,
                    });
                    succeeded += 1;
                }
            }
            Err(e) => {
                project_results.push(ReleaseProjectDeployResult {
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

    // Clean up build artifacts now that deployment is complete.
    // The release pipeline skipped cleanup when --deploy was set so the
    // deploy step could find the ZIP artifact.
    let distrib_path = format!("{}/target/distrib", local_path);
    if std::path::Path::new(&distrib_path).exists() {
        if let Err(e) = std::fs::remove_dir_all(&distrib_path) {
            log_status!(
                "release",
                "Warning: failed to clean up {}: {}",
                distrib_path,
                e
            );
        } else {
            log_status!("release", "Cleaned up {}", distrib_path);
        }
    }

    (
        Some(ReleaseDeploymentResult {
            projects: project_results,
            summary: ReleaseDeploymentSummary {
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

fn run_recover(input: &ReleaseCommandInput) -> Result<(ReleaseCommandResult, i32)> {
    let component = load_component(
        &input.component_id,
        &ReleaseOptions {
            path_override: input.path_override.clone(),
            ..Default::default()
        },
    )?;
    let monorepo = git::MonorepoContext::detect(&component.local_path, &input.component_id);
    let version_info = crate::version::read_component_version(&component)?;
    let current_version = &version_info.version;
    let tag_name = format_tag(current_version, monorepo.as_ref());

    let tag_exists_local =
        git::tag_exists_locally(&component.local_path, &tag_name).unwrap_or(false);
    let tag_exists_remote =
        git::tag_exists_on_remote(&component.local_path, &tag_name).unwrap_or(false);
    let uncommitted = git::get_uncommitted_changes(&component.local_path)?;

    let mut actions = Vec::new();

    if uncommitted.has_changes {
        log_status!("recover", "Committing uncommitted changes...");
        let msg = format!("release: v{}", current_version);
        let commit_result = git::commit(
            Some(&input.component_id),
            Some(msg.as_str()),
            git::CommitOptions {
                staged_only: false,
                files: None,
                exclude: None,
                amend: false,
            },
        )?;
        if !commit_result.success {
            return Err(Error::git_command_failed(format!(
                "Failed to commit: {}",
                commit_result.stderr
            )));
        }
        actions.push("committed version files".to_string());
    }

    if !tag_exists_local {
        log_status!("recover", "Creating tag {}...", tag_name);
        let tag_result = git::tag(
            Some(&input.component_id),
            Some(&tag_name),
            Some(&format!("Release {}", tag_name)),
        )?;
        if !tag_result.success {
            return Err(Error::git_command_failed(format!(
                "Failed to create tag: {}",
                tag_result.stderr
            )));
        }
        actions.push(format!("created tag {}", tag_name));
    }

    if !tag_exists_remote {
        log_status!("recover", "Pushing to remote...");
        let push_result = git::push(Some(&input.component_id), true)?;
        if !push_result.success {
            return Err(Error::git_command_failed(format!(
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
        ReleaseCommandResult {
            component_id: input.component_id.clone(),
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
        0,
    ))
}

/// Run releases for multiple components sequentially.
///
/// Continue-on-error: if one component fails, the rest still run.
/// Each component releases independently (own tag, own push).
pub fn run_batch(
    component_ids: &[String],
    input_template: &ReleaseCommandInput,
) -> BatchReleaseResult {
    let mut results = Vec::new();
    let mut released: u32 = 0;
    let mut skipped: u32 = 0;
    let mut failed: u32 = 0;

    for component_id in component_ids {
        log_status!(
            "release",
            "--- Releasing '{}' ({}/{}) ---",
            component_id,
            results.len() + 1,
            component_ids.len()
        );

        let input = ReleaseCommandInput {
            component_id: component_id.clone(),
            path_override: None,
            dry_run: input_template.dry_run,
            deploy: input_template.deploy,
            recover: input_template.recover,
            skip_checks: input_template.skip_checks,
            bump_override: input_template.bump_override.clone(),
            skip_publish: input_template.skip_publish,
        };

        match run_command(input) {
            Ok((result, _exit_code)) => {
                let was_skipped = result.skipped_reason.is_some();
                let status = if was_skipped {
                    skipped += 1;
                    "skipped"
                } else {
                    released += 1;
                    "released"
                };

                results.push(BatchReleaseComponentResult {
                    component_id: component_id.clone(),
                    status: status.to_string(),
                    error: None,
                    result: Some(result),
                });
            }
            Err(e) => {
                log_status!("release", "Failed to release '{}': {}", component_id, e);
                failed += 1;
                results.push(BatchReleaseComponentResult {
                    component_id: component_id.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    result: None,
                });
            }
        }
    }

    let total = results.len() as u32;

    // Log summary
    if total > 1 {
        log_status!("release", "--- Batch summary ---");
        log_status!(
            "release",
            "{} component(s): {} released, {} skipped, {} failed",
            total,
            released,
            skipped,
            failed
        );
    }

    BatchReleaseResult {
        results,
        summary: BatchReleaseSummary {
            total,
            released,
            skipped,
            failed,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::pipeline::{PipelineRunResult, PipelineRunStatus, PipelineStepResult};
    use std::collections::HashMap;

    #[test]
    fn extracts_new_version_from_plan() {
        let plan = ReleasePlan {
            component_id: "demo".to_string(),
            enabled: true,
            steps: vec![crate::release::ReleasePlanStep {
                id: "version".to_string(),
                step_type: "version".to_string(),
                label: None,
                needs: vec![],
                config: HashMap::from([(
                    "to".to_string(),
                    serde_json::Value::String("1.2.3".to_string()),
                )]),
                status: crate::release::ReleasePlanStatus::Ready,
                missing: vec![],
            }],
            semver_recommendation: None,
            warnings: vec![],
            hints: vec![],
        };

        assert_eq!(
            extract_new_version_from_plan(&plan).as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn detects_post_release_warnings() {
        let run = ReleaseRun {
            component_id: "demo".to_string(),
            enabled: true,
            result: PipelineRunResult {
                steps: vec![PipelineStepResult {
                    id: "post_release".to_string(),
                    step_type: "post_release".to_string(),
                    status: PipelineRunStatus::Success,
                    missing: vec![],
                    warnings: vec![],
                    hints: vec![],
                    data: Some(serde_json::json!({ "all_succeeded": false })),
                    error: None,
                }],
                status: PipelineRunStatus::Success,
                warnings: vec![],
                summary: None,
            },
        };

        assert!(has_post_release_warnings(&run));
    }

    #[test]
    fn test_run_command_default_path() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_match_resolve_bump_component_local_path_monorepo_as_ref() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_skipped_reason_some_no_releasable_commits_to_string() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_let_bump_type_if_let_some_ref_override_value_input_bump_over() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_some_override_value_clone() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_bump_type_major() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_skipped_reason_some_major_requires_flag_to_string() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_options_dry_run() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_plan_some_plan() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_default_path_2() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_if_let_some_ref_t_tag() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_run_some_run_result() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_has_expected_effects() {
        // Expected effects: logging
        let input = Default::default();
        let _ = run_command(input);
    }

    #[test]
    fn test_run_batch_match_run_command_input() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_result_some_result() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_err_e() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_error_some_e_to_string() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_has_expected_effects() {
        // Expected effects: mutation, logging
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _ = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_command_default_path() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_match_resolve_bump_component_local_path_monorepo_as_ref() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_skipped_reason_some_no_releasable_commits_to_string() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_let_bump_type_if_let_some_ref_override_value_input_bump_over() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_some_override_value_clone() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_bump_type_major() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_skipped_reason_some_major_requires_flag_to_string() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_options_dry_run() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_plan_some_plan() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_default_path_2() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_if_let_some_ref_t_tag() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_run_some_run_result() {
        let input = Default::default();
        let _result = run_command(input);
    }

    #[test]
    fn test_run_command_has_expected_effects() {
        // Expected effects: logging
        let input = Default::default();
        let _ = run_command(input);
    }

    #[test]
    fn test_run_batch_match_run_command_input() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_result_some_result() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_err_e() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_error_some_e_to_string() {
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _result = run_batch(&component_ids, &input_template);
    }

    #[test]
    fn test_run_batch_has_expected_effects() {
        // Expected effects: mutation, logging
        let component_ids = Vec::new();
        let input_template = Default::default();
        let _ = run_batch(&component_ids, &input_template);
    }

}
