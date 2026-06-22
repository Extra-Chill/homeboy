use std::collections::HashSet;
use std::path::Path;

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::plan::{PlanStep, PlanValues};

use super::context::{load_component, resolve_extensions};
use super::execution_dispatch::{
    execute_release_plan_step, release_step_is_show_stopper, ReleaseExecutionContext,
};
use super::plan_steps::build_preflight_steps;
use super::types::{ReleaseOptions, ReleasePlan, ReleaseState, ReleaseStepResult};

pub(super) fn build_initial_preflight_plan(
    component_id: &str,
    options: &ReleaseOptions,
) -> ReleasePlan {
    let mut steps: Vec<PlanStep> = build_preflight_steps(options, None, &[])
        .into_iter()
        .filter(|step| initial_executable_preflight_ids().contains(&step.id.as_str()))
        .collect();

    if let Some(tag_step) = early_tag_availability_step(options, false) {
        let insert_at = steps
            .iter()
            .position(|step| step.id == "preflight.lint")
            .unwrap_or(steps.len());
        steps.insert(insert_at, tag_step);
    }

    ReleasePlan::new(component_id, true, steps, None, Vec::new(), Vec::new())
}

pub(super) fn build_dry_run_preflight_plan(
    component_id: &str,
    options: &ReleaseOptions,
) -> ReleasePlan {
    let mut steps: Vec<PlanStep> = build_preflight_steps(options, None, &[])
        .into_iter()
        .filter(|step| dry_run_executable_preflight_ids().contains(&step.id.as_str()))
        .collect();

    if let Some(tag_step) = early_tag_availability_step(options, true) {
        steps.push(tag_step);
    }

    ReleasePlan::new(component_id, true, steps, None, Vec::new(), Vec::new())
}

pub(super) fn initial_executable_preflight_ids() -> &'static [&'static str] {
    &[
        "preflight.default_branch",
        "preflight.git_identity",
        "preflight.working_tree",
        "preflight.remote_sync",
        "preflight.tag_availability",
        "preflight.lint",
        "preflight.test",
        "preflight.changelog_bootstrap",
    ]
}

fn dry_run_executable_preflight_ids() -> &'static [&'static str] {
    &["preflight.default_branch", "preflight.working_tree"]
}

fn early_tag_availability_step(options: &ReleaseOptions, dry_run: bool) -> Option<PlanStep> {
    if options.pipeline.head || options.bump_type == "none" {
        return None;
    }

    let needs = if dry_run {
        vec!["preflight.working_tree".to_string()]
    } else {
        vec!["preflight.remote_sync".to_string()]
    };

    Some(PlanStep::ready_labeled(
        "preflight.tag_availability",
        "preflight.tag_availability",
        "Check release tag is available",
        needs,
        PlanValues::new(),
    ))
}

pub(super) fn execute_plan_steps(
    steps: &[PlanStep],
    component_id: &str,
    options: &ReleaseOptions,
    results: &mut Vec<ReleaseStepResult>,
    skip_step_ids: &HashSet<&'static str>,
) -> Result<bool> {
    if steps.is_empty() {
        return Ok(false);
    }

    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component)?;
    let mut context = ReleaseExecutionContext {
        component: &component,
        extensions: &extensions,
        component_id,
        options,
        state: initial_release_state(&component, component_id, options)?,
        publish_failed: false,
    };

    let run = crate::core::execution::execute_plan_steps_filtered(
        steps,
        |step| skip_step_ids.contains(step.id.as_str()),
        |step| execute_release_plan_step(step, &mut context),
        release_step_is_show_stopper,
    )?;
    results.extend(run.results);

    Ok(run.stopped)
}

fn initial_release_state(
    component: &Component,
    component_id: &str,
    options: &ReleaseOptions,
) -> Result<ReleaseState> {
    if !options.pipeline.head {
        return Ok(ReleaseState::default());
    }

    let version_info = super::version::read_component_version(component)?;
    let monorepo = git::MonorepoContext::detect(&component.local_path, component_id);
    let expected_tag = match monorepo.as_ref() {
        Some(ctx) => ctx.format_tag(&version_info.version),
        None => format!("v{}", version_info.version),
    };

    let tag = resolve_head_tag(&component.local_path, &expected_tag)?;
    let notes = read_current_release_notes(component)?;

    Ok(ReleaseState {
        version: Some(version_info.version),
        tag: Some(tag),
        notes,
        artifacts: Vec::new(),
        changelog_validation: None,
    })
}

fn resolve_head_tag(local_path: &str, expected_tag: &str) -> Result<String> {
    let output = git::execute_git_for_release(local_path, &["tag", "--points-at", "HEAD"])
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to inspect tags pointing at HEAD: {}", e),
                Some("git tag --points-at HEAD".to_string()),
            )
        })?;

    let tags: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    if tags.iter().any(|tag| tag == expected_tag) {
        return Ok(expected_tag.to_string());
    }

    Err(Error::validation_invalid_argument(
        "head",
        format!(
            "--head requires tag '{}' to point at HEAD; found: {}",
            expected_tag,
            if tags.is_empty() {
                "none".to_string()
            } else {
                tags.join(", ")
            }
        ),
        Some(expected_tag.to_string()),
        Some(vec![
            "Check out the release tag or push the expected tag before running `homeboy release --head`.".to_string(),
        ]),
    ))
}

fn read_current_release_notes(component: &Component) -> Result<Option<String>> {
    let changelog = component
        .changelog_target
        .as_deref()
        .unwrap_or("CHANGELOG.md");
    let path = Path::new(&component.local_path).join(changelog);
    if !path.exists() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&path).map_err(|e| {
        Error::internal_io(
            format!("Failed to read changelog release notes: {}", e),
            Some(path.display().to_string()),
        )
    })?;

    Ok(super::utils::extract_latest_notes(&content))
}

#[cfg(test)]
mod tests {
    use super::{
        build_dry_run_preflight_plan, execute_plan_steps, initial_executable_preflight_ids,
    };
    use crate::core::plan::PlanStepStatus;
    use crate::core::release::types::{ReleaseOptions, ReleaseStepStatus};
    use std::collections::HashSet;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn test_initial_executable_preflight_ids() {
        assert_eq!(
            initial_executable_preflight_ids(),
            &[
                "preflight.default_branch",
                "preflight.git_identity",
                "preflight.working_tree",
                "preflight.remote_sync",
                "preflight.tag_availability",
                "preflight.lint",
                "preflight.test",
                "preflight.changelog_bootstrap",
            ]
        );
    }

    #[test]
    fn initial_preflight_plan_checks_tag_availability_before_quality_gates() {
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            ..Default::default()
        };
        let plan = super::build_initial_preflight_plan("fixture", &options);
        let steps = plan.plan.steps;
        let tag = steps
            .iter()
            .position(|step| step.id == "preflight.tag_availability")
            .expect("tag preflight");
        let lint = steps
            .iter()
            .position(|step| step.id == "preflight.lint")
            .expect("lint preflight");
        let test = steps
            .iter()
            .position(|step| step.id == "preflight.test")
            .expect("test preflight");

        assert!(tag < lint, "tag availability must fail before lint runs");
        assert!(tag < test, "tag availability must fail before tests run");
        assert_eq!(steps[tag].needs, vec!["preflight.remote_sync"]);
    }

    #[test]
    fn test_build_initial_preflight_plan() {
        let options = ReleaseOptions {
            bump_type: "none".to_string(),
            ..Default::default()
        };

        let plan = super::build_initial_preflight_plan(
            "missing-component-is-not-loaded-without-tag-check",
            &options,
        );
        let ids: Vec<&str> = plan
            .plan
            .steps
            .iter()
            .map(|step| step.id.as_str())
            .collect();

        assert_eq!(
            ids,
            vec![
                "preflight.default_branch",
                "preflight.git_identity",
                "preflight.working_tree",
                "preflight.remote_sync",
                "preflight.lint",
                "preflight.test",
                "preflight.changelog_bootstrap",
            ]
        );
        assert!(plan.semver_recommendation().is_none());
        assert!(plan
            .plan
            .steps
            .iter()
            .any(|step| step.id == "preflight.git_identity"
                && step.status == PlanStepStatus::Disabled));
    }

    #[test]
    fn test_build_dry_run_preflight_plan_only_includes_non_mutating_guards() {
        let options = ReleaseOptions {
            bump_type: "none".to_string(),
            ..Default::default()
        };

        let plan = build_dry_run_preflight_plan(
            "missing-component-is-not-loaded-without-tag-check",
            &options,
        );
        let ids: Vec<&str> = plan
            .plan
            .steps
            .iter()
            .map(|step| step.id.as_str())
            .collect();

        assert_eq!(
            ids,
            vec!["preflight.default_branch", "preflight.working_tree"]
        );
    }

    #[test]
    fn dry_run_preflight_refuses_remote_existing_release_tag() {
        let (_remote, checkout) = release_repo_with_remote_tag("0.11.10", "v0.11.11");
        let options = ReleaseOptions {
            bump_type: "patch".to_string(),
            path_override: Some(checkout.path().to_string_lossy().to_string()),
            dry_run: true,
            ..Default::default()
        };

        let plan = build_dry_run_preflight_plan("fixture", &options);
        let mut results = Vec::new();
        let stopped = execute_plan_steps(
            &plan.plan.steps,
            "fixture",
            &options,
            &mut results,
            &HashSet::new(),
        )
        .expect("dry-run preflights should execute");

        assert!(stopped, "existing remote tag must stop dry-run preflights");
        let tag_result = results
            .iter()
            .find(|result| result.id == "preflight.tag_availability")
            .expect("tag availability preflight should run");
        assert_eq!(tag_result.status, ReleaseStepStatus::Failed);
        assert!(
            tag_result
                .data
                .as_ref()
                .and_then(|data| data.get("remote_tag"))
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "remote tag commit should be reported in structured data"
        );
        assert!(
            tag_result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Release tag v0.11.11 already exists"),
            "expected existing tag failure, got: {:?}",
            tag_result.error
        );
    }

    #[test]
    fn test_execute_plan_steps() {
        let mut results = Vec::new();

        let stopped = execute_plan_steps(
            &[],
            "missing-component-is-not-loaded-for-empty-plan",
            &ReleaseOptions::default(),
            &mut results,
            &HashSet::new(),
        )
        .expect("empty plan should be a no-op");

        assert!(!stopped);
        assert!(results.is_empty());
    }

    fn release_repo_with_remote_tag(
        version: &str,
        tag: &str,
    ) -> (tempfile::TempDir, tempfile::TempDir) {
        let remote = tempfile::tempdir().expect("remote tempdir");
        let checkout = tempfile::tempdir().expect("checkout tempdir");
        run_in(
            remote.path(),
            &["git", "init", "--bare", "-b", "main", "-q"],
        );
        run_in(
            checkout.path(),
            &[
                "git",
                "clone",
                "-q",
                remote.path().to_str().expect("remote path"),
                ".",
            ],
        );
        run_in(
            checkout.path(),
            &["git", "config", "user.email", "t@example.com"],
        );
        run_in(checkout.path(), &["git", "config", "user.name", "T"]);
        run_in(
            checkout.path(),
            &["git", "config", "commit.gpgsign", "false"],
        );

        std::fs::write(
            checkout.path().join("plugin.php"),
            format!("<?php\n/*\nVersion: {}\n*/\n", version),
        )
        .expect("write plugin file");
        std::fs::write(
            checkout.path().join("homeboy.json"),
            r#"{
                "id": "fixture-project",
                "components": {
                    "fixture": {
                        "type": "wordpress-plugin",
                        "path": ".",
                        "version_targets": [
                            {"file": "plugin.php", "pattern": "(?m)^Version:\\s*([0-9.]+)"}
                        ]
                    }
                }
            }"#,
        )
        .expect("write homeboy config");
        run_in(checkout.path(), &["git", "add", "."]);
        run_in(
            checkout.path(),
            &["git", "commit", "-q", "-m", "feat: fixture"],
        );
        run_in(checkout.path(), &["git", "push", "-q", "origin", "main"]);
        run_in(checkout.path(), &["git", "tag", tag]);
        run_in(checkout.path(), &["git", "push", "-q", "origin", tag]);
        run_in(checkout.path(), &["git", "tag", "-d", tag]);

        (remote, checkout)
    }

    fn run_in(dir: &Path, args: &[&str]) {
        let output = Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .expect("spawn command");
        assert!(
            output.status.success(),
            "command {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
