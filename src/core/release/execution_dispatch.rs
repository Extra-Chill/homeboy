use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::extension::ExtensionManifest;
use crate::core::git;
use crate::core::plan::{PlanStep, PlanStepStatus};
use crate::core::release::executor;
use crate::core::release::types::{
    ReleaseOptions, ReleaseState, ReleaseStepResult, ReleaseStepStatus,
};
use std::collections::BTreeSet;

pub(super) struct ReleaseExecutionContext<'a> {
    pub(super) component: &'a Component,
    pub(super) extensions: &'a [ExtensionManifest],
    pub(super) component_id: &'a str,
    pub(super) options: &'a ReleaseOptions,
    pub(super) state: ReleaseState,
    pub(super) publish_failed: bool,
}

pub(super) fn execute_release_plan_step(
    step: &PlanStep,
    context: &mut ReleaseExecutionContext,
) -> Result<Option<ReleaseStepResult>> {
    if matches!(step.status, PlanStepStatus::Disabled) || release_step_is_plan_only(step) {
        return Ok(None);
    }

    let tracked_dirty_before = tracked_dirty_snapshot_for_step(step, context)?;
    let result = match step.kind.as_str() {
        "preflight.default_branch" => Ok(Some(run_default_branch_preflight(step, context))),
        "preflight.git_identity" => configure_git_identity(step, context).map(Some),
        "preflight.working_tree" => Ok(Some(run_working_tree_preflight(step, context))),
        "preflight.remote_sync" => Ok(Some(run_remote_sync_preflight(step, context))),
        "preflight.bump_policy" => Ok(Some(run_bump_policy_preflight(step))),
        "preflight.lint" => Ok(Some(run_lint_preflight(step, context))),
        "preflight.test" => Ok(Some(run_test_preflight(step, context))),
        "preflight.changelog_bootstrap" => {
            Ok(Some(run_changelog_bootstrap_preflight(step, context)))
        }
        "preflight.package" => Ok(Some(
            executor::package_preflight::run_package_preflight(
                context.extensions,
                context.component_id,
                &context.component.local_path,
                context.options.skip_build_validation,
            )
            .unwrap_or_else(|err| failed_result("preflight.package", "preflight.package", err)),
        )),
        "preflight.tag_availability" => {
            let tag_name = step
                .inputs
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            executor::run_tag_availability_preflight(
                context.component,
                context.component_id,
                tag_name,
            )
            .map(Some)
        }
        step_kind if step_kind.starts_with("preflight.extension.") => {
            Ok(Some(executor::run_extension_release_preflight(
                step,
                context.extensions,
                &context.state,
                context.component_id,
                &context.component.local_path,
            )))
        }
        "changelog.finalize" => {
            executor::changelog::run_changelog_finalize(step, context.component, &mut context.state)
                .map(Some)
        }
        "version" => executor::run_version(
            context.component,
            &mut context.state,
            step.inputs
                .get("bump")
                .and_then(|value| value.as_str())
                .unwrap_or(&context.options.bump_type),
        )
        .map(Some),
        "release.prepare" => Ok(Some(
            executor::prepare::run_prepare(
                context.extensions,
                &context.state,
                context.component_id,
                &context.component.local_path,
            )
            .unwrap_or_else(|err| failed_result("release.prepare", "release.prepare", err)),
        )),
        "git.commit" => {
            executor::run_git_commit(context.component, context.component_id, &context.state)
                .map(Some)
        }
        "package" => Ok(Some(
            executor::run_package(
                context.extensions,
                &mut context.state,
                context.component_id,
                &context.component.local_path,
                context.options.skip_build_validation,
            )
            .unwrap_or_else(|err| failed_result("package", "package", err)),
        )),
        "artifacts.inventory" => {
            let dir = step
                .inputs
                .get("dir")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            Ok(Some(
                executor::artifacts::run_artifact_inventory(&mut context.state, dir)
                    .unwrap_or_else(|err| {
                        failed_result("artifacts.inventory", "artifacts.inventory", err)
                    }),
            ))
        }
        "git.tag" => {
            let tag_name = step
                .inputs
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("v{}", context.state.version.as_deref().unwrap_or("")));
            executor::run_git_tag(
                context.component,
                context.component_id,
                &mut context.state,
                &tag_name,
            )
            .map(Some)
        }
        "git.push" => executor::run_git_push(context.component, context.component_id).map(Some),
        "github.release" => Ok(Some(
            executor::run_github_release(context.component, &context.state)
                .unwrap_or_else(|err| failed_result("github.release", "github.release", err)),
        )),
        "cleanup" => {
            if context.publish_failed {
                return Ok(None);
            }
            Ok(Some(
                executor::run_cleanup(context.component, &context.state)
                    .unwrap_or_else(|err| failed_result("cleanup", "cleanup", err)),
            ))
        }
        "post_release" => {
            let commands = step_config_string_array(step, "commands");
            Ok(Some(
                executor::run_post_release(context.component, &commands)
                    .unwrap_or_else(|err| failed_result("post_release", "post_release", err)),
            ))
        }
        "deploy" => Ok(Some(super::deployment::run_deployment_step(
            context.component_id,
            &context.component.local_path,
            context.state.version.as_deref(),
            &context.state.artifacts,
        ))),
        step_kind if step_kind.starts_with("publish.") => {
            let target = step_kind.strip_prefix("publish.").unwrap_or_default();
            let result = executor::run_publish(
                context.extensions,
                &context.state,
                context.component_id,
                &context.component.local_path,
                Some(&context.component.github),
                target,
            )
            .unwrap_or_else(|err| {
                context.publish_failed = true;
                failed_result(step_kind, step_kind, err)
            });

            if matches!(result.status, ReleaseStepStatus::Failed) {
                context.publish_failed = true;
            }

            Ok(Some(result))
        }
        _ => Err(Error::internal_unexpected(format!(
            "release plan contains unsupported executable step '{}'",
            step.kind
        ))),
    }?;

    if let Some(result) = result {
        return guard_step_dirty_side_effects(step, context, tracked_dirty_before, result)
            .map(Some);
    }

    Ok(None)
}

fn release_step_is_plan_only(step: &PlanStep) -> bool {
    (step.kind.starts_with("preflight.")
        && step.kind != "preflight.default_branch"
        && step.kind != "preflight.git_identity"
        && step.kind != "preflight.working_tree"
        && step.kind != "preflight.remote_sync"
        && step.kind != "preflight.bump_policy"
        && step.kind != "preflight.lint"
        && step.kind != "preflight.test"
        && step.kind != "preflight.changelog_bootstrap"
        && step.kind != "preflight.package"
        && step.kind != "preflight.tag_availability"
        && !step.kind.starts_with("preflight.extension."))
        || step.kind == "changelog.policy"
        || step.kind == "changelog.generate"
}

fn run_default_branch_preflight(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
) -> ReleaseStepResult {
    match super::planning_git::validate_default_branch(context.component) {
        Ok(()) => ReleaseStepResult {
            id: step.id.clone(),
            step_type: step.kind.clone(),
            status: ReleaseStepStatus::Success,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints: Vec::new(),
            data: None,
            error: None,
        },
        Err(err) => failed_result(&step.id, &step.kind, err),
    }
}

fn run_working_tree_preflight(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
) -> ReleaseStepResult {
    match super::planning_worktree::validate_working_tree_fail_fast(context.component) {
        Ok(()) => ReleaseStepResult {
            id: step.id.clone(),
            step_type: step.kind.clone(),
            status: ReleaseStepStatus::Success,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints: Vec::new(),
            data: None,
            error: None,
        },
        Err(err) => failed_result(&step.id, &step.kind, err),
    }
}

fn run_remote_sync_preflight(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
) -> ReleaseStepResult {
    match super::planning_git::validate_remote_sync(context.component) {
        Ok(()) => ReleaseStepResult {
            id: step.id.clone(),
            step_type: step.kind.clone(),
            status: ReleaseStepStatus::Success,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints: Vec::new(),
            data: None,
            error: None,
        },
        Err(err) => failed_result(&step.id, &step.kind, err),
    }
}

fn run_bump_policy_preflight(step: &PlanStep) -> ReleaseStepResult {
    let requested = step
        .inputs
        .get("requested")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let recommended = step
        .inputs
        .get("recommended")
        .and_then(|value| value.as_str())
        .unwrap_or(requested);
    let underbump = step
        .inputs
        .get("underbump")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let force_lower_bump = step
        .inputs
        .get("force_lower_bump")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    if underbump && !force_lower_bump {
        let err = Error::validation_invalid_argument(
            "bump",
            format!(
                "Requested {} bump is lower than detected {} impact",
                requested, recommended
            ),
            Some(requested.to_string()),
            None,
        )
        .with_hint(format!("Use the detected bump: --bump {}", recommended))
        .with_hint("If the lower release is intentional, re-run with --force-lower-bump");

        return failed_result(&step.id, &step.kind, err);
    }

    ReleaseStepResult {
        id: step.id.clone(),
        step_type: step.kind.clone(),
        status: ReleaseStepStatus::Success,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints: Vec::new(),
        data: Some(serde_json::json!({
            "requested": requested,
            "recommended": recommended,
            "underbump": underbump,
            "force_lower_bump": force_lower_bump,
        })),
        error: None,
    }
}

fn run_lint_preflight(step: &PlanStep, context: &ReleaseExecutionContext) -> ReleaseStepResult {
    use super::planning_quality::LintQualityOutcome;

    match super::planning_quality::validate_lint_quality(context.component) {
        LintQualityOutcome::Passed { ran } => successful_quality_result(step, ran),
        LintQualityOutcome::Failed(err) => failed_result(&step.id, &step.kind, err),
        LintQualityOutcome::HarnessError { message } => {
            log_status!("release", "Lint harness warning: {}", message);
            ReleaseStepResult {
                id: step.id.clone(),
                step_type: step.kind.clone(),
                status: ReleaseStepStatus::Success,
                missing: Vec::new(),
                warnings: vec![message],
                hints: Vec::new(),
                data: Some(serde_json::json!({ "ran": true, "harness_error": true })),
                error: None,
            }
        }
    }
}

fn run_test_preflight(step: &PlanStep, context: &ReleaseExecutionContext) -> ReleaseStepResult {
    match super::planning_quality::validate_test_quality(context.component) {
        Ok(ran) => successful_quality_result(step, ran),
        Err(err) => failed_result(&step.id, &step.kind, err),
    }
}

fn successful_quality_result(step: &PlanStep, ran: bool) -> ReleaseStepResult {
    ReleaseStepResult {
        id: step.id.clone(),
        step_type: step.kind.clone(),
        status: ReleaseStepStatus::Success,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints: Vec::new(),
        data: Some(serde_json::json!({ "ran": ran })),
        error: None,
    }
}

fn run_changelog_bootstrap_preflight(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
) -> ReleaseStepResult {
    match super::planning_changelog::ensure_changelog_initialized(context.component) {
        Ok(()) => ReleaseStepResult {
            id: step.id.clone(),
            step_type: step.kind.clone(),
            status: ReleaseStepStatus::Success,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints: Vec::new(),
            data: None,
            error: None,
        },
        Err(err) => failed_result(&step.id, &step.kind, err),
    }
}

fn configure_git_identity(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
) -> Result<ReleaseStepResult> {
    let identity_value = step
        .inputs
        .get("identity")
        .and_then(|value| value.as_str())
        .ok_or_else(|| Error::internal_unexpected("release git identity step missing identity"))?;
    let identity = git::parse_git_identity(Some(identity_value));
    git::configure_identity(&context.component.local_path, &identity)?;
    log_status!(
        "release",
        "Git identity: {} <{}>",
        identity.name,
        identity.email
    );

    Ok(ReleaseStepResult {
        id: step.id.clone(),
        step_type: step.kind.clone(),
        status: ReleaseStepStatus::Success,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints: Vec::new(),
        data: Some(serde_json::json!({
            "name": identity.name,
            "email": identity.email,
        })),
        error: None,
    })
}

pub(super) fn release_step_is_show_stopper(result: &ReleaseStepResult) -> bool {
    if !matches!(result.status, ReleaseStepStatus::Failed) {
        return false;
    }

    matches!(
        result.step_type.as_str(),
        "changelog.finalize"
            | "preflight.default_branch"
            | "preflight.working_tree"
            | "preflight.remote_sync"
            | "preflight.bump_policy"
            | "preflight.lint"
            | "preflight.test"
            | "preflight.changelog_bootstrap"
            | "preflight.package"
            | "preflight.tag_availability"
            | "version"
            | "release.prepare"
            | "git.commit"
            | "package"
            | "git.tag"
            | "git.push"
            // A failed github.release means the GitHub Release object was not
            // created (or its assets were not attached). Halt before
            // publish/upload steps run against a non-existent release (#3541).
            | "github.release"
    )
}

fn step_config_string_array(step: &PlanStep, key: &str) -> Vec<String> {
    step.inputs
        .get(key)
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Convert a step error into a failed `ReleaseStepResult`.
fn failed_result(id: &str, step_type: &str, err: Error) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Failed,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints: err.hints.clone(),
        data: Some(serde_json::json!({ "error_details": err.details })),
        error: Some(err.message),
    }
}

fn tracked_dirty_snapshot_for_step(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
) -> Result<Option<BTreeSet<String>>> {
    if !release_step_dirty_side_effects_are_guarded(step) {
        return Ok(None);
    }
    if !git::is_git_repo(&context.component.local_path) {
        return Ok(None);
    }

    tracked_dirty_snapshot(&context.component.local_path).map(Some)
}

fn guard_step_dirty_side_effects(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
    before: Option<BTreeSet<String>>,
    result: ReleaseStepResult,
) -> Result<ReleaseStepResult> {
    if !matches!(result.status, ReleaseStepStatus::Success) {
        return Ok(result);
    }

    let Some(before) = before else {
        return Ok(result);
    };

    let after = tracked_dirty_snapshot(&context.component.local_path)?;
    let introduced: Vec<String> = after.difference(&before).cloned().collect();
    let introduced = release_step_unexpected_dirty_files(step, context, introduced)?;
    if introduced.is_empty() {
        return Ok(result);
    }

    Ok(dirty_side_effect_failure(step, introduced))
}

fn release_step_dirty_side_effects_are_guarded(step: &PlanStep) -> bool {
    matches!(
        step.kind.as_str(),
        "preflight.lint" | "preflight.test" | "preflight.package" | "release.prepare" | "package"
    )
}

fn release_step_unexpected_dirty_files(
    step: &PlanStep,
    context: &ReleaseExecutionContext,
    files: Vec<String>,
) -> Result<Vec<String>> {
    if step.kind != "release.prepare" {
        return Ok(files);
    }

    let allowed = release_prepare_allowed_dirty_files(context.component)?;
    Ok(files
        .into_iter()
        .filter(|file| !allowed.iter().any(|allowed| file == allowed))
        .collect())
}

fn release_prepare_allowed_dirty_files(
    component: &crate::core::component::Component,
) -> Result<Vec<String>> {
    let changelog_path = super::changelog::resolve_changelog_path(component)?;
    let version_targets = component
        .version_targets
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|target| {
            std::path::Path::new(&component.local_path)
                .join(&target.file)
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>();

    Ok(super::planning_worktree::get_release_allowed_files(
        &changelog_path,
        &version_targets,
        std::path::Path::new(&component.local_path),
    ))
}

fn tracked_dirty_snapshot(path: &str) -> Result<BTreeSet<String>> {
    let changes = git::get_uncommitted_changes(path)?;
    let files = changes
        .staged
        .into_iter()
        .chain(changes.unstaged)
        .collect::<Vec<_>>();
    Ok(super::planning_worktree::filter_homeboy_managed(files)
        .into_iter()
        .collect())
}

fn dirty_side_effect_failure(step: &PlanStep, files: Vec<String>) -> ReleaseStepResult {
    let file_count = files.len();
    let displayed_files = files.iter().take(10).cloned().collect::<Vec<_>>();
    let suffix = if file_count > 10 { ", ..." } else { "" };
    let err = Error::validation_invalid_argument(
        "working_tree",
        format!(
            "{} dirtied tracked file{}: {}{}",
            step.kind,
            if file_count == 1 { "" } else { "s" },
            displayed_files.join(", "),
            suffix,
        ),
        Some(step.kind.clone()),
        Some(vec![
            "Commit intentional generated asset changes before retrying the release".to_string(),
            "Fix the build/test/package step so it is deterministic and leaves tracked files unchanged".to_string(),
            "Restore generated output with `git restore <file>` before rerunning if the change was not intentional".to_string(),
        ]),
    )
    .with_hint("Commit intentional generated asset changes before retrying the release")
    .with_hint(
        "Fix the build/test/package step so it is deterministic and leaves tracked files unchanged",
    )
    .with_hint(
        "Restore generated output with `git restore <file>` before rerunning if the change was not intentional",
    );
    let mut result = failed_result(&step.id, &step.kind, err);
    result.data = Some(serde_json::json!({
        "phase": step.kind,
        "dirty_tracked_files": files,
        "recovery": [
            "Commit intentional generated asset changes before retrying the release",
            "Fix the build/test/package step so it is deterministic and leaves tracked files unchanged",
            "Restore generated output with `git restore <file>` before rerunning if the change was not intentional"
        ]
    }));
    result
}

#[cfg(test)]
mod tests {
    use super::{
        execute_release_plan_step, release_step_is_plan_only, release_step_is_show_stopper,
        release_step_unexpected_dirty_files, ReleaseExecutionContext,
    };
    use crate::core::component::{Component, ComponentScriptsConfig, VersionTarget};
    use crate::core::plan::PlanStep;
    use crate::core::release::types::{
        ReleaseOptions, ReleaseState, ReleaseStepResult, ReleaseStepStatus,
    };

    #[test]
    fn test_release_step_is_plan_only() {
        let steps = [
            plan_step("preflight.audit"),
            plan_step("changelog.policy"),
            plan_step("changelog.generate"),
        ];

        assert!(steps.iter().all(release_step_is_plan_only));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.default_branch"
        )));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.git_identity"
        )));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.working_tree"
        )));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.remote_sync"
        )));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.bump_policy"
        )));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.extension.registry.publish_token"
        )));
        assert!(!release_step_is_plan_only(&plan_step("preflight.lint")));
        assert!(!release_step_is_plan_only(&plan_step("preflight.test")));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.changelog_bootstrap"
        )));
        assert!(!release_step_is_plan_only(&plan_step("preflight.package")));
        assert!(!release_step_is_plan_only(&plan_step("changelog.finalize")));
        assert!(!release_step_is_plan_only(&plan_step("deploy")));
    }

    #[test]
    fn test_execute_release_plan_step() {
        let component = Component {
            id: "fixture".to_string(),
            local_path: "/tmp/fixture".to_string(),
            ..Default::default()
        };
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: true,
        };
        let step = plan_step("cleanup");

        let result = execute_release_plan_step(&step, &mut context).expect("dispatch");
        assert!(result.is_none());
    }

    #[test]
    fn preflight_default_branch_returns_failed_step_on_feature_branch() {
        let temp = tempfile::tempdir().expect("tempdir");
        run_in(temp.path(), &["git", "init", "-q"]);
        run_in(
            temp.path(),
            &["git", "config", "user.email", "test@example.com"],
        );
        run_in(temp.path(), &["git", "config", "user.name", "Test"]);
        std::fs::write(temp.path().join("README.md"), "fixture\n").expect("write fixture");
        run_in(temp.path(), &["git", "add", "."]);
        run_in(
            temp.path(),
            &["git", "commit", "-q", "-m", "Initial commit"],
        );
        run_in(temp.path(), &["git", "checkout", "-q", "-b", "feature"]);

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Default::default()
        };
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let result =
            execute_release_plan_step(&plan_step("preflight.default_branch"), &mut context)
                .expect("dispatch")
                .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(release_step_is_show_stopper(&result));
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("non-default branch"));
    }

    #[test]
    fn preflight_working_tree_returns_success_step_for_clean_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        run_in(temp.path(), &["git", "init", "-q"]);
        run_in(
            temp.path(),
            &["git", "config", "user.email", "test@example.com"],
        );
        run_in(temp.path(), &["git", "config", "user.name", "Test"]);
        std::fs::write(temp.path().join("README.md"), "fixture\n").expect("write fixture");
        run_in(temp.path(), &["git", "add", "."]);
        run_in(
            temp.path(),
            &["git", "commit", "-q", "-m", "Initial commit"],
        );

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Default::default()
        };
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let result = execute_release_plan_step(&plan_step("preflight.working_tree"), &mut context)
            .expect("dispatch")
            .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(!release_step_is_show_stopper(&result));
    }

    #[test]
    fn preflight_remote_sync_fast_forwards_and_returns_success_step() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_in(
            temp.path(),
            &[
                "git",
                "init",
                "--bare",
                "--initial-branch",
                "main",
                &remote_str,
            ],
        );
        run_in(temp.path(), &["git", "clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_in(&seed, &["git", "add", "."]);
        run_in(&seed, &["git", "commit", "-q", "-m", "Initial commit"]);
        run_in(&seed, &["git", "push", "-q", "origin", "main"]);

        run_in(temp.path(), &["git", "clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);

        std::fs::write(seed.join("README.md"), "fixture\nsecond\n").expect("write update");
        run_in(&seed, &["git", "add", "."]);
        run_in(&seed, &["git", "commit", "-q", "-m", "Second commit"]);
        run_in(&seed, &["git", "push", "-q", "origin", "main"]);

        let component = Component {
            id: "fixture".to_string(),
            local_path: checkout.to_string_lossy().to_string(),
            ..Default::default()
        };
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let result = execute_release_plan_step(&plan_step("preflight.remote_sync"), &mut context)
            .expect("dispatch")
            .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(!release_step_is_show_stopper(&result));
        assert_eq!(
            run_output(&checkout, &["git", "rev-parse", "HEAD"]),
            run_output(&checkout, &["git", "rev-parse", "origin/main"])
        );
    }

    #[test]
    fn quality_preflights_return_success_when_no_runner_is_available() {
        let temp = tempfile::tempdir().expect("tempdir");
        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Default::default()
        };
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        for step_type in ["preflight.lint", "preflight.test"] {
            let result = execute_release_plan_step(&plan_step(step_type), &mut context)
                .expect("dispatch")
                .expect("result");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            assert_eq!(result.data, Some(serde_json::json!({ "ran": false })));
            assert!(!release_step_is_show_stopper(&result));
        }
    }

    #[test]
    fn extension_declared_release_preflight_executes_action() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let mut extension: crate::core::extension::ExtensionManifest =
                serde_json::from_value(serde_json::json!({
                    "name": "Registry",
                    "version": "1.0.0",
                    "release_preflights": [{
                        "id": "publish_token",
                        "label": "Validate registry publish token",
                        "action": "release.preflight.publish-token",
                        "needs": ["preflight.bump_policy"]
                    }],
                    "actions": [{
                        "id": "release.preflight.publish-token",
                        "label": "Validate publish token",
                        "type": "command",
                        "command": "printf '{\"component\":\"%s\",\"path\":\"%s\",\"success\":true}' \"$HOMEBOY_COMPONENT_ID\" \"$HOMEBOY_COMPONENT_PATH\""
                    }]
                }))
                .expect("extension manifest");
            extension.id = "registry".to_string();
            crate::core::extension::save_manifest(&extension).expect("save extension");

            let component = Component {
                id: "fixture".to_string(),
                local_path: temp.path().to_string_lossy().to_string(),
                ..Default::default()
            };
            let options = ReleaseOptions::default();
            let extensions = vec![extension];
            let mut context = ReleaseExecutionContext {
                component: &component,
                extensions: &extensions,
                component_id: "fixture",
                options: &options,
                state: ReleaseState::default(),
                publish_failed: false,
            };
            let mut step = plan_step("preflight.extension.registry.publish_token");
            step.inputs
                .insert("extension".to_string(), serde_json::json!("registry"));
            step.inputs.insert(
                "action".to_string(),
                serde_json::json!("release.preflight.publish-token"),
            );

            let result = execute_release_plan_step(&step, &mut context)
                .expect("dispatch")
                .expect("result");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            let stdout = result
                .data
                .as_ref()
                .and_then(|data| data.get("response"))
                .and_then(|response| response.get("stdout"))
                .and_then(serde_json::Value::as_str)
                .expect("preflight stdout");
            let payload: serde_json::Value = serde_json::from_str(stdout).expect("stdout json");
            assert_eq!(payload["component"], "fixture");
            assert_eq!(payload["path"], temp.path().to_string_lossy().as_ref());
        });
    }

    #[test]
    fn quality_preflight_reports_phase_that_dirties_tracked_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        run_in(temp.path(), &["git", "init", "-q"]);
        configure_git_user(temp.path());
        std::fs::write(temp.path().join("tracked.txt"), "clean\n").expect("write tracked file");
        std::fs::create_dir(temp.path().join("scripts")).expect("create scripts dir");
        std::fs::write(
            temp.path().join("scripts/test.sh"),
            "printf 'dirty\\n' > tracked.txt\n",
        )
        .expect("write test script");
        run_in(temp.path(), &["git", "add", "."]);
        run_in(
            temp.path(),
            &["git", "commit", "-q", "-m", "Initial commit"],
        );

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            scripts: Some(ComponentScriptsConfig {
                test: vec!["sh scripts/test.sh".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let result = execute_release_plan_step(&plan_step("preflight.test"), &mut context)
            .expect("dispatch")
            .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(release_step_is_show_stopper(&result));
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("preflight.test dirtied tracked file"));
        assert_eq!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("phase"))
                .and_then(|value| value.as_str()),
            Some("preflight.test")
        );
        assert!(result
            .data
            .as_ref()
            .and_then(|data| data.get("dirty_tracked_files"))
            .and_then(|value| value.as_array())
            .expect("dirty tracked files should be reported")
            .iter()
            .any(|value| value.as_str() == Some("tracked.txt")));
        assert!(result
            .hints
            .iter()
            .any(|hint| hint.message.contains("Fix the build/test/package step")));
    }

    #[test]
    fn release_prepare_dirty_guard_allows_release_owned_lockfiles() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let component = Component {
            id: "fixture".to_string(),
            local_path: root.to_string_lossy().to_string(),
            changelog_target: Some("docs/changelog.md".to_string()),
            version_targets: Some(vec![VersionTarget {
                file: "Cargo.toml".to_string(),
                pattern: None,
                artifact_path: None,
            }]),
            ..Default::default()
        };
        let options = ReleaseOptions::default();
        let context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let unexpected = release_step_unexpected_dirty_files(
            &plan_step("release.prepare"),
            &context,
            vec![
                "Cargo.lock".to_string(),
                "Cargo.toml".to_string(),
                "docs/changelog.md".to_string(),
                "src/lib.rs".to_string(),
            ],
        )
        .expect("guard should classify dirty files");

        assert_eq!(unexpected, vec!["src/lib.rs"]);
    }

    #[test]
    fn bump_policy_preflight_blocks_unforced_underbump() {
        let mut step = plan_step("preflight.bump_policy");
        step.inputs
            .insert("requested".to_string(), serde_json::json!("patch"));
        step.inputs
            .insert("recommended".to_string(), serde_json::json!("minor"));
        step.inputs
            .insert("underbump".to_string(), serde_json::json!(true));
        step.inputs
            .insert("force_lower_bump".to_string(), serde_json::json!(false));

        let component = Component::default();
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let result = execute_release_plan_step(&step, &mut context)
            .expect("dispatch")
            .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(release_step_is_show_stopper(&result));
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Requested patch bump is lower"));
        assert!(result
            .hints
            .iter()
            .any(|hint| hint.message.contains("--force-lower-bump")));
    }

    #[test]
    fn bump_policy_preflight_allows_forced_underbump() {
        let mut step = plan_step("preflight.bump_policy");
        step.inputs
            .insert("requested".to_string(), serde_json::json!("patch"));
        step.inputs
            .insert("recommended".to_string(), serde_json::json!("minor"));
        step.inputs
            .insert("underbump".to_string(), serde_json::json!(true));
        step.inputs
            .insert("force_lower_bump".to_string(), serde_json::json!(true));

        let component = Component::default();
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let result = execute_release_plan_step(&step, &mut context)
            .expect("dispatch")
            .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert_eq!(
            result.data,
            Some(serde_json::json!({
                "requested": "patch",
                "recommended": "minor",
                "underbump": true,
                "force_lower_bump": true,
            }))
        );
    }

    #[test]
    fn changelog_bootstrap_preflight_initializes_missing_changelog() {
        let temp = tempfile::tempdir().expect("tempdir");
        let changelog_path = temp.path().join("CHANGELOG.md");
        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            changelog_target: Some("CHANGELOG.md".to_string()),
            ..Default::default()
        };
        let options = ReleaseOptions::default();
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };

        let result =
            execute_release_plan_step(&plan_step("preflight.changelog_bootstrap"), &mut context)
                .expect("dispatch")
                .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(changelog_path.exists());
    }

    #[test]
    fn version_step_uses_planned_bump_input() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("plugin.php"),
            "<?php\n/*\nPlugin Name: Fixture\nVersion: 0.6.12\n*/\n",
        )
        .expect("write plugin");
        std::fs::write(
            temp.path().join("CHANGELOG.md"),
            "# Changelog\n\n## Unreleased\n\n- Planned bump fixture\n",
        )
        .expect("write changelog");
        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            changelog_target: Some("CHANGELOG.md".to_string()),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"(?:Version|version)[:=]\s+([0-9]+\.[0-9]+\.[0-9]+)".to_string()),
                artifact_path: None,
            }]),
            ..Default::default()
        };
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };
        let mut context = ReleaseExecutionContext {
            component: &component,
            extensions: &[],
            component_id: "fixture",
            options: &options,
            state: ReleaseState::default(),
            publish_failed: false,
        };
        let mut step = plan_step("version");
        step.inputs
            .insert("bump".to_string(), serde_json::json!("minor"));

        let result = execute_release_plan_step(&step, &mut context)
            .expect("dispatch")
            .expect("result");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert_eq!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("new_version"))
                .and_then(|value| value.as_str()),
            Some("0.7.0")
        );
        let plugin = std::fs::read_to_string(temp.path().join("plugin.php")).expect("read plugin");
        assert!(plugin.contains("Version: 0.7.0"));
    }

    #[test]
    fn test_release_step_is_show_stopper() {
        let version_failure = failed_step_result("version");
        let default_branch_failure = failed_step_result("preflight.default_branch");
        let working_tree_failure = failed_step_result("preflight.working_tree");
        let remote_sync_failure = failed_step_result("preflight.remote_sync");
        let bump_policy_failure = failed_step_result("preflight.bump_policy");
        let lint_failure = failed_step_result("preflight.lint");
        let test_failure = failed_step_result("preflight.test");
        let bootstrap_failure = failed_step_result("preflight.changelog_bootstrap");
        let package_preflight_failure = failed_step_result("preflight.package");
        let changelog_failure = failed_step_result("changelog.finalize");
        let github_release_failure = failed_step_result("github.release");
        let publish_failure = failed_step_result("publish.crates");

        assert!(release_step_is_show_stopper(&version_failure));
        // #3541: a failed github.release must halt the plan before
        // publish/upload steps run against a non-existent release.
        assert!(release_step_is_show_stopper(&github_release_failure));
        assert!(release_step_is_show_stopper(&default_branch_failure));
        assert!(release_step_is_show_stopper(&working_tree_failure));
        assert!(release_step_is_show_stopper(&remote_sync_failure));
        assert!(release_step_is_show_stopper(&bump_policy_failure));
        assert!(release_step_is_show_stopper(&lint_failure));
        assert!(release_step_is_show_stopper(&test_failure));
        assert!(release_step_is_show_stopper(&bootstrap_failure));
        assert!(release_step_is_show_stopper(&package_preflight_failure));
        assert!(release_step_is_show_stopper(&changelog_failure));
        assert!(!release_step_is_show_stopper(&publish_failure));
    }

    #[test]
    fn test_step_config_string_array() {
        let mut step = plan_step("post_release");
        step.inputs.insert(
            "commands".to_string(),
            serde_json::json!(["git tag -f stable", 123, "git push"]),
        );

        assert_eq!(
            super::step_config_string_array(&step, "commands"),
            vec!["git tag -f stable", "git push"]
        );
    }

    #[test]
    fn test_failed_result() {
        let err = crate::core::error::Error::internal_unexpected("boom".to_string());

        let result = super::failed_result("package", "package", err);

        assert_eq!(result.id, "package");
        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert_eq!(result.error.as_deref(), Some("boom"));
    }

    fn plan_step(step_type: &str) -> PlanStep {
        PlanStep::ready(step_type, step_type).build()
    }

    fn failed_step_result(step_type: &str) -> ReleaseStepResult {
        ReleaseStepResult {
            id: step_type.to_string(),
            step_type: step_type.to_string(),
            status: ReleaseStepStatus::Failed,
            missing: vec![],
            warnings: vec![],
            hints: vec![],
            data: None,
            error: Some("failed".to_string()),
        }
    }

    fn run_in(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .expect("spawn command");
        assert!(
            output.status.success(),
            "command {:?} failed: stdout={:?} stderr={:?}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn run_output(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .expect("spawn command");
        assert!(
            output.status.success(),
            "command {:?} failed: stdout={:?} stderr={:?}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn configure_git_user(dir: &std::path::Path) {
        run_in(dir, &["git", "config", "user.email", "test@example.com"]);
        run_in(dir, &["git", "config", "user.name", "Test"]);
    }
}
