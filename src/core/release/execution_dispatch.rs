use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::extension::ExtensionManifest;
use crate::core::git;
use crate::core::plan::{PlanStep, PlanStepStatus};
use crate::core::release::executor;
use crate::core::release::types::{
    ReleaseOptions, ReleaseState, ReleaseStepResult, ReleaseStepStatus,
};

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

    match step.kind.as_str() {
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
                executor::run_cleanup(context.component)
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
        ))),
        step_kind if step_kind.starts_with("publish.") => {
            let target = step_kind.strip_prefix("publish.").unwrap_or_default();
            let result = executor::run_publish(
                context.extensions,
                &context.state,
                context.component,
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
    }
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
        && step.kind != "preflight.changelog_bootstrap")
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
    match super::planning_quality::validate_lint_quality(context.component) {
        Ok(ran) => successful_quality_result(step, ran),
        Err(err) => failed_result(&step.id, &step.kind, err),
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
            | "version"
            | "release.prepare"
            | "git.commit"
            | "package"
            | "git.tag"
            | "git.push"
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

#[cfg(test)]
mod tests {
    use super::{
        execute_release_plan_step, release_step_is_plan_only, release_step_is_show_stopper,
        ReleaseExecutionContext,
    };
    use crate::core::component::{Component, VersionTarget};
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
        assert!(!release_step_is_plan_only(&plan_step("preflight.lint")));
        assert!(!release_step_is_plan_only(&plan_step("preflight.test")));
        assert!(!release_step_is_plan_only(&plan_step(
            "preflight.changelog_bootstrap"
        )));
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
        let changelog_failure = failed_step_result("changelog.finalize");
        let publish_failure = failed_step_result("publish.crates");

        assert!(release_step_is_show_stopper(&version_failure));
        assert!(release_step_is_show_stopper(&default_branch_failure));
        assert!(release_step_is_show_stopper(&working_tree_failure));
        assert!(release_step_is_show_stopper(&remote_sync_failure));
        assert!(release_step_is_show_stopper(&bump_policy_failure));
        assert!(release_step_is_show_stopper(&lint_failure));
        assert!(release_step_is_show_stopper(&test_failure));
        assert!(release_step_is_show_stopper(&bootstrap_failure));
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
