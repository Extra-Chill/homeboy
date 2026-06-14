use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::plan::PlanStep;

use super::context::load_component;
use super::types::{
    BatchReleaseComponentResult, BatchReleaseResult, BatchReleaseSummary, ReleaseBumpPolicyOptions,
    ReleaseCommandInput, ReleaseCommandResult, ReleaseOptions, ReleasePlan, ReleaseRun,
    ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus,
};

pub fn run_command(input: ReleaseCommandInput) -> Result<(ReleaseCommandResult, i32)> {
    if input.recover {
        return run_recover(&input);
    }

    if input.pipeline.from_artifacts.is_some() && !input.pipeline.head {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            "--from-artifacts requires --head",
            input.pipeline.from_artifacts.clone(),
            None,
        ));
    }

    if input.pipeline.head && input.bump_override.is_some() {
        return Err(Error::validation_invalid_argument(
            "bump",
            "--head uses the version already present at HEAD and cannot be combined with --bump",
            input.bump_override.clone(),
            None,
        ));
    }

    let component = load_component(
        &input.component_id,
        &ReleaseOptions {
            path_override: input.path_override.clone(),
            ..Default::default()
        },
    )?;

    let monorepo = git::MonorepoContext::detect(&component.local_path, &input.component_id);
    let resolved_bump = if input.pipeline.head {
        None
    } else {
        resolve_bump(&component.local_path, monorepo.as_ref())?
    };
    let (auto_bump_type, releasable_count) = resolved_bump
        .clone()
        .unwrap_or_else(|| ("none".to_string(), 0));

    let has_breaking_commits = auto_bump_type == "major";

    // Resolve the effective bump type: --bump overrides auto-detection.
    let bump_type = if input.pipeline.head {
        "head".to_string()
    } else if let Some(ref override_value) = input.bump_override {
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

            if resolved_bump.is_some() {
                log_status!(
                    "release",
                    "Using --bump {} (overriding auto-detected {} from {} commit{})",
                    bump,
                    auto_bump_type,
                    releasable_count,
                    if releasable_count == 1 { "" } else { "s" }
                );
            }
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

        if bump_type != "none" {
            log_status!(
                "release",
                "Detected {} bump from {} releasable commit{}",
                bump_type,
                releasable_count,
                if releasable_count == 1 { "" } else { "s" }
            );
        }

        bump_type
    };

    let require_explicit_major = input.bump_override.is_none() && bump_type == "major";

    let options = ReleaseOptions {
        bump_type: bump_type.clone(),
        dry_run: input.dry_run,
        path_override: input.path_override,
        skip_checks: input.skip_checks,
        pipeline: input.pipeline.clone(),
        skip_github_release: input.skip_github_release,
        git_identity: input.git_identity.clone(),
        bump_policy: ReleaseBumpPolicyOptions {
            force_lower_bump: input.force_lower_bump,
            force_empty_release: input.bump_override.is_some(),
            require_explicit_major,
        },
    };

    if options.dry_run {
        let dry_run_preflight = run_dry_run_preflights(&input.component_id, &options)?;
        if let Some(run) = dry_run_preflight {
            let plan = super::plan(&input.component_id, &options).ok();
            let skipped_reason = plan.as_ref().and_then(skipped_reason_from_plan);
            let status = release_command_status(true, skipped_reason.as_deref(), Some(&run));
            let release_summary = release_summary_from_run(&run);
            return Ok((
                ReleaseCommandResult {
                    component_id: input.component_id,
                    status,
                    bump_type,
                    dry_run: true,
                    releasable_commits: releasable_count,
                    new_version: None,
                    tag: None,
                    skipped_reason,
                    plan,
                    run: Some(run),
                    deployment: None,
                    release_summary,
                },
                1,
            ));
        }

        let plan = super::plan(&input.component_id, &options)?;
        let new_version = if input.pipeline.head {
            current_component_version(&component)?
        } else {
            extract_new_version_from_plan(&plan)
        };
        let tag = new_version
            .as_ref()
            .map(|v| format_tag(v, monorepo.as_ref()));
        let deployment = dry_run_deployment_plan(&input.component_id, input.pipeline.deploy, &plan);

        return Ok((
            ReleaseCommandResult {
                component_id: input.component_id,
                status: release_command_status(
                    true,
                    skipped_reason_from_plan(&plan).as_deref(),
                    None,
                ),
                bump_type,
                dry_run: true,
                releasable_commits: releasable_count,
                new_version,
                tag,
                skipped_reason: skipped_reason_from_plan(&plan),
                plan: Some(plan),
                run: None,
                deployment,
                release_summary: release_summary_for_skipped_plan(),
            },
            0,
        ));
    }

    let (plan, run_result) = super::pipeline::run_with_plan(&input.component_id, &options)?;
    display_release_summary(&run_result);

    let new_version = if input.pipeline.head {
        current_component_version(&component)?
    } else {
        extract_new_version_from_run(&run_result)
    };
    let tag = new_version
        .as_ref()
        .map(|v| format_tag(v, monorepo.as_ref()));
    let release_step_exit = release_run_failure_exit(&run_result);
    let post_release_exit = if has_post_release_warnings(&run_result) {
        3
    } else {
        0
    };
    let deployment = super::deployment::extract_deployment_from_run(&run_result);
    let skipped_reason = skipped_reason_from_plan(&plan);
    let release_summary = release_summary_from_run(&run_result);
    display_release_outcome_summary(&release_summary);
    let deploy_exit_code = deployment
        .as_ref()
        .filter(|deployment| deployment.summary.failed > 0)
        .map(|_| 1)
        .unwrap_or(0);
    let exit_code = if release_step_exit != 0 {
        release_step_exit
    } else if deploy_exit_code != 0 {
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
            status: release_command_status(false, skipped_reason.as_deref(), Some(&run_result)),
            bump_type,
            dry_run: false,
            releasable_commits: releasable_count,
            new_version,
            tag,
            skipped_reason,
            plan: Some(plan),
            run: Some(run_result),
            deployment,
            release_summary,
        },
        exit_code,
    ))
}

fn run_dry_run_preflights(
    component_id: &str,
    options: &ReleaseOptions,
) -> Result<Option<ReleaseRun>> {
    use std::collections::HashSet;

    let preflight_plan = super::execution_plan::build_dry_run_preflight_plan(component_id, options);
    let mut results = Vec::new();
    let stopped = super::execution_plan::execute_plan_steps(
        &preflight_plan.plan.steps,
        component_id,
        options,
        &mut results,
        &HashSet::new(),
    )?;

    if stopped || results.iter().any(step_failed) {
        return Ok(Some(ReleaseRun {
            component_id: component_id.to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: results,
                status: ReleaseStepStatus::Failed,
                warnings: Vec::new(),
                summary: None,
            },
        }));
    }

    Ok(None)
}

fn step_failed(step: &ReleaseStepResult) -> bool {
    matches!(
        step.status,
        ReleaseStepStatus::Failed | ReleaseStepStatus::Missing
    )
}

fn dry_run_deployment_plan(
    component_id: &str,
    deploy_requested: bool,
    plan: &ReleasePlan,
) -> Option<super::types::ReleaseDeploymentResult> {
    if deploy_requested && plan.enabled() {
        Some(super::deployment::plan_deployment(component_id))
    } else {
        None
    }
}

fn current_component_version(
    component: &crate::core::component::Component,
) -> Result<Option<String>> {
    super::version::read_component_version(component).map(|info| Some(info.version))
}

fn resolve_bump(
    local_path: &str,
    monorepo: Option<&git::MonorepoContext>,
) -> Result<Option<(String, usize)>> {
    let (_latest_tag, commits) =
        super::planning_semver::resolve_tag_and_commits(local_path, monorepo)?;

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

fn short_sha(commit: &str) -> &str {
    &commit[..8.min(commit.len())]
}

fn extract_new_version_from_plan(plan: &ReleasePlan) -> Option<String> {
    plan.plan
        .steps
        .iter()
        .find(|s| s.kind == "version")
        .and_then(|s| s.inputs.get("to"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn skipped_reason_from_plan(plan: &ReleasePlan) -> Option<String> {
    if plan.enabled() {
        return None;
    }

    plan.plan
        .steps
        .iter()
        .find(|step| step.id == "release.skip")
        .and_then(|step| step.inputs.get("reason"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
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

fn display_release_outcome_summary(summary: &[String]) {
    if summary.is_empty() {
        return;
    }

    eprintln!();
    for line in summary {
        log_status!("release", "{}", line);
    }
}

fn release_command_status(
    dry_run: bool,
    skipped_reason: Option<&str>,
    run: Option<&ReleaseRun>,
) -> String {
    if skipped_reason.is_some() {
        return "skipped".to_string();
    }
    if dry_run {
        return "planned".to_string();
    }
    match run.map(|run| &run.result.status) {
        Some(ReleaseStepStatus::Success) => "released".to_string(),
        Some(ReleaseStepStatus::PartialSuccess) => "partial".to_string(),
        Some(ReleaseStepStatus::Failed) => "failed".to_string(),
        Some(ReleaseStepStatus::Missing) => "missing".to_string(),
        Some(ReleaseStepStatus::Skipped) => "skipped".to_string(),
        None => "unknown".to_string(),
    }
}

fn release_summary_for_skipped_plan() -> Vec<String> {
    vec![
        "No release commit created".to_string(),
        "No tag created".to_string(),
        "No GitHub Release created".to_string(),
    ]
}

fn release_summary_from_run(run: &ReleaseRun) -> Vec<String> {
    let mut summary = Vec::new();

    if matches!(run.result.status, ReleaseStepStatus::Success) {
        if let Some(line) = release_created_line(run) {
            summary.push(line);
        }
    }

    summary.push(git_commit_summary_line(run));
    summary.push(git_tag_summary_line(run));
    summary.push(github_release_summary_line(run));
    summary
}

fn release_created_line(run: &ReleaseRun) -> Option<String> {
    let tag = successful_step(run, "git.tag")
        .and_then(|step| step.data.as_ref())
        .and_then(|data| data.get("tag"))
        .and_then(|value| value.as_str());
    let url = successful_step(run, "github.release")
        .and_then(|step| step.data.as_ref())
        .and_then(|data| data.get("url"))
        .and_then(|value| value.as_str());

    match (tag, url) {
        (Some(tag), Some(url)) => Some(format!("Release created: {} ({})", tag, url)),
        (Some(tag), None) => Some(format!("Release created: {}", tag)),
        (None, Some(url)) => Some(format!("Release created: {}", url)),
        (None, None) => None,
    }
}

fn git_commit_summary_line(run: &ReleaseRun) -> String {
    let Some(step) = successful_step(run, "git.commit") else {
        return "No release commit created".to_string();
    };
    if step_data_bool(step, "skipped") {
        "No release commit created".to_string()
    } else {
        "Release commit created".to_string()
    }
}

fn git_tag_summary_line(run: &ReleaseRun) -> String {
    let Some(step) = successful_step(run, "git.tag") else {
        return "No tag created".to_string();
    };
    let tag = step
        .data
        .as_ref()
        .and_then(|data| data.get("tag"))
        .and_then(|value| value.as_str());
    if step_data_bool(step, "skipped") {
        tag.map(|tag| format!("Tag already exists: {}", tag))
            .unwrap_or_else(|| "No tag created".to_string())
    } else {
        tag.map(|tag| format!("Tag created: {}", tag))
            .unwrap_or_else(|| "Tag created".to_string())
    }
}

fn github_release_summary_line(run: &ReleaseRun) -> String {
    let Some(step) = successful_step(run, "github.release") else {
        return "No GitHub Release created".to_string();
    };
    if step_data_bool(step, "skipped") {
        return "No GitHub Release created".to_string();
    }
    step.data
        .as_ref()
        .and_then(|data| data.get("url"))
        .and_then(|value| value.as_str())
        .map(|url| format!("GitHub Release created: {}", url))
        .unwrap_or_else(|| "GitHub Release created".to_string())
}

fn successful_step<'a>(run: &'a ReleaseRun, step_type: &str) -> Option<&'a ReleaseStepResult> {
    run.result.steps.iter().find(|step| {
        step.step_type == step_type && matches!(step.status, ReleaseStepStatus::Success)
    })
}

fn step_data_bool(step: &ReleaseStepResult, key: &str) -> bool {
    step.data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
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

fn release_run_failure_exit(run: &ReleaseRun) -> i32 {
    match run.result.status {
        ReleaseStepStatus::Success | ReleaseStepStatus::Skipped => 0,
        ReleaseStepStatus::PartialSuccess
        | ReleaseStepStatus::Failed
        | ReleaseStepStatus::Missing => 1,
    }
}

fn run_recover(input: &ReleaseCommandInput) -> Result<(ReleaseCommandResult, i32)> {
    let component = load_component(
        &input.component_id,
        &ReleaseOptions {
            path_override: input.path_override.clone(),
            ..Default::default()
        },
    )?;

    // Configure git identity for recovery commits/tags
    if let Some(ref identity_str) = input.git_identity {
        let identity = git::parse_git_identity(Some(identity_str));
        git::configure_identity(&component.local_path, &identity)?;
    }

    let monorepo = git::MonorepoContext::detect(&component.local_path, &input.component_id);
    let version_info = crate::core::release::version::read_component_version(&component)?;
    let current_version = &version_info.version;
    let tag_name = format_tag(current_version, monorepo.as_ref());

    // Surface the orphan-tag pattern from issue #2234. When the latest release
    // tag points at a commit whose subject is *not* `release: vX.Y.Z`, the
    // previous release was botched (tag without bump). Recover should warn
    // loudly so the operator can decide whether to delete the orphan tag, hand
    // back-fill a release: commit, or run `--recover` to commit the version
    // files at the tagged commit.
    if let Some(latest_tag) = latest_release_tag(&component.local_path, monorepo.as_ref()) {
        if let Some(diagnostic) = diagnose_orphan_tag(&component.local_path, &latest_tag) {
            log_status!("recover", "{}", diagnostic);
        }
    }

    let tag_exists_local =
        git::tag_exists_locally(&component.local_path, &tag_name).unwrap_or(false);
    let tag_exists_remote =
        git::tag_exists_on_remote(&component.local_path, &tag_name).unwrap_or(false);
    let head_commit = git::get_head_commit(&component.local_path)?;
    let local_tag_commit = if tag_exists_local {
        Some(git::get_tag_commit(&component.local_path, &tag_name)?)
    } else {
        None
    };
    let remote_tag_commit = git::remote_tag_commit(&component.local_path, &tag_name)?;

    if local_tag_commit
        .as_deref()
        .is_some_and(|commit| commit != head_commit)
        || remote_tag_commit
            .as_deref()
            .is_some_and(|commit| commit != head_commit)
    {
        return Err(Error::validation_invalid_argument(
            "tag",
            format!("Tag '{}' exists but does not point to HEAD", tag_name),
            Some(format!(
                "local tag points to {}, origin tag points to {}, HEAD is {}",
                local_tag_commit
                    .as_deref()
                    .map(short_sha)
                    .unwrap_or("<missing>"),
                remote_tag_commit
                    .as_deref()
                    .map(short_sha)
                    .unwrap_or("<missing>"),
                short_sha(&head_commit)
            )),
            Some(vec![
                format!(
                    "Inspect the existing tag before recovery: git show --no-patch --decorate {}",
                    tag_name
                ),
                format!(
                    "If the existing tag is valid, create a new releasable commit and run: homeboy release {}",
                    input.component_id
                ),
                format!(
                    "If the tag is an abandoned partial release, delete the GitHub release/tag explicitly, then run: homeboy release {} --recover",
                    input.component_id
                ),
            ]),
        ));
    }

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
        let push_result = git::push(
            Some(&input.component_id),
            git::PushOptions {
                tags: true,
                force_with_lease: false,
                ..Default::default()
            },
        )?;
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
            status: if actions.is_empty() {
                "already_recovered".to_string()
            } else {
                "recovered".to_string()
            },
            bump_type: "recover".to_string(),
            dry_run: false,
            releasable_commits: 0,
            new_version: None,
            tag: Some(tag_name.clone()),
            skipped_reason: None,
            plan: Some(recovery_release_plan(
                &input.component_id,
                current_version,
                &tag_name,
                uncommitted.has_changes,
                !tag_exists_local,
                !tag_exists_remote,
                &actions,
            )),
            run: None,
            deployment: None,
            release_summary: if actions.is_empty() {
                vec![format!("Release already exists: {}", tag_name)]
            } else {
                actions.to_vec()
            },
        },
        0,
    ))
}

fn recovery_release_plan(
    component_id: &str,
    version: &str,
    tag_name: &str,
    commit_needed: bool,
    tag_needed: bool,
    push_needed: bool,
    actions: &[String],
) -> ReleasePlan {
    let mut steps = Vec::new();
    steps.push(recovery_step(
        "recover.commit",
        "Commit recovery changes",
        commit_needed,
        vec![],
    ));
    steps.push(recovery_step(
        "recover.tag",
        format!("Create tag {}", tag_name),
        tag_needed,
        vec!["recover.commit".to_string()],
    ));
    steps.push(recovery_step(
        "recover.push",
        "Push recovery state",
        push_needed,
        vec!["recover.tag".to_string()],
    ));

    for step in &mut steps {
        step.inputs.insert(
            "version".to_string(),
            serde_json::Value::String(version.to_string()),
        );
        step.inputs.insert(
            "tag".to_string(),
            serde_json::Value::String(tag_name.to_string()),
        );
    }

    ReleasePlan::new(
        component_id,
        !actions.is_empty(),
        steps,
        None,
        Vec::new(),
        actions.to_vec(),
    )
}

fn recovery_step(id: &str, label: impl Into<String>, needed: bool, needs: Vec<String>) -> PlanStep {
    if needed {
        PlanStep::ready_labeled(id, id, label, needs, std::iter::empty())
    } else {
        PlanStep::disabled_with_reason(id, id, "already-complete")
            .label(label)
            .needs(needs)
            .build()
    }
}

/// Resolve the most recent release-shaped tag for the component, honoring
/// monorepo prefixes. Returns `None` if no matching tag is found.
fn latest_release_tag(local_path: &str, monorepo: Option<&git::MonorepoContext>) -> Option<String> {
    match monorepo {
        Some(ctx) => git::get_latest_tag_with_prefix(&ctx.git_root, Some(&ctx.tag_prefix)).ok()?,
        None => git::get_latest_tag(local_path).ok()?,
    }
}

/// Inspect the latest release tag for the orphan-tag pattern (#2234): a tag
/// whose tagged commit subject is not `release: vX.Y.Z`. Returns a one-line
/// warning when the tag looks orphaned, otherwise `None`.
///
/// This is intentionally a soft warning — `--recover` may still be the
/// right move (re-commit the working tree), but the operator deserves to
/// know they're recovering on top of a misplaced tag before they push more
/// state to origin.
fn diagnose_orphan_tag(local_path: &str, tag: &str) -> Option<String> {
    let tag_commit = git::get_tag_commit(local_path, tag).ok()?;
    let subject_output =
        git::execute_git_for_release(local_path, &["log", "-1", "--format=%s", &tag_commit])
            .ok()?;
    if !subject_output.status.success() {
        return None;
    }
    let subject = String::from_utf8_lossy(&subject_output.stdout)
        .trim()
        .to_string();

    if subject.starts_with("release: v") || subject.starts_with("release:v") {
        return None;
    }

    Some(format!(
        "⚠ Latest tag {} points at commit {} ({}) — not a `release: v...` commit. \
         This matches the orphan-tag pattern from issue #2234. Inspect the tag/commit before recovering: \
         `git show {}`. To delete a misplaced tag locally and on origin: \
         `git tag -d {} && git push origin :refs/tags/{}`",
        tag,
        &tag_commit[..8.min(tag_commit.len())],
        subject,
        tag,
        tag,
        tag,
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
            recover: input_template.recover,
            skip_checks: input_template.skip_checks,
            bump_override: input_template.bump_override.clone(),
            force_lower_bump: input_template.force_lower_bump,
            pipeline: input_template.pipeline.clone(),
            skip_github_release: input_template.skip_github_release,
            git_identity: input_template.git_identity.clone(),
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
    use crate::core::plan::{PlanStep, PlanStepStatus, PlanValues};
    use crate::core::release::{ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus};

    #[test]
    fn extracts_new_version_from_plan() {
        let plan = ReleasePlan::new(
            "demo",
            true,
            vec![PlanStep::ready("version", "version")
                .inputs(PlanValues::new().string("to", "1.2.3"))
                .build()],
            None,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            extract_new_version_from_plan(&plan).as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn dry_run_deployment_plan_is_omitted_when_release_plan_is_disabled() {
        let plan = ReleasePlan::new(
            "demo",
            false,
            vec![PlanStep::disabled_with_reason(
                "release.skip",
                "release.skip",
                "no-releasable-commits",
            )
            .build()],
            None,
            Vec::new(),
            Vec::new(),
        );

        assert!(dry_run_deployment_plan("demo", true, &plan).is_none());
    }

    #[test]
    fn recovery_release_plan_marks_needed_steps_ready() {
        let actions = vec![
            "committed version files".to_string(),
            "created tag v1.2.3".to_string(),
        ];
        let plan = recovery_release_plan("demo", "1.2.3", "v1.2.3", true, true, false, &actions);

        assert!(plan.enabled());
        assert_eq!(plan.component_id(), Some("demo"));
        assert_eq!(plan.plan.hints, actions);
        assert_eq!(plan.plan.steps.len(), 3);
        assert_eq!(plan.plan.steps[0].id, "recover.commit");
        assert_eq!(plan.plan.steps[0].status, PlanStepStatus::Ready);
        assert_eq!(plan.plan.steps[1].id, "recover.tag");
        assert_eq!(plan.plan.steps[1].status, PlanStepStatus::Ready);
        assert_eq!(plan.plan.steps[2].id, "recover.push");
        assert_eq!(plan.plan.steps[2].status, PlanStepStatus::Disabled);
        assert_eq!(
            plan.plan.steps[2]
                .inputs
                .get("reason")
                .and_then(|v| v.as_str()),
            Some("already-complete")
        );
        assert_eq!(
            plan.plan.steps[0]
                .inputs
                .get("version")
                .and_then(|v| v.as_str()),
            Some("1.2.3")
        );
        assert_eq!(
            plan.plan.steps[0]
                .inputs
                .get("tag")
                .and_then(|v| v.as_str()),
            Some("v1.2.3")
        );
    }

    #[test]
    fn recovery_release_plan_is_disabled_when_nothing_needed() {
        let plan = recovery_release_plan("demo", "1.2.3", "v1.2.3", false, false, false, &[]);

        assert!(!plan.enabled());
        assert!(plan.plan.hints.is_empty());
        assert!(plan
            .plan
            .steps
            .iter()
            .all(|step| step.status == PlanStepStatus::Disabled));
    }

    #[test]
    fn detects_post_release_warnings() {
        let run = ReleaseRun {
            component_id: "demo".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![ReleaseStepResult {
                    id: "post_release".to_string(),
                    step_type: "post_release".to_string(),
                    status: ReleaseStepStatus::Success,
                    missing: vec![],
                    warnings: vec![],
                    hints: vec![],
                    data: Some(serde_json::json!({ "all_succeeded": false })),
                    error: None,
                }],
                status: ReleaseStepStatus::Success,
                warnings: vec![],
                summary: None,
            },
        };

        assert!(has_post_release_warnings(&run));
    }

    #[test]
    fn release_run_failure_exit_fails_partial_release_runs() {
        let run = ReleaseRun {
            component_id: "demo".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![ReleaseStepResult {
                    id: "git.push".to_string(),
                    step_type: "git.push".to_string(),
                    status: ReleaseStepStatus::Failed,
                    missing: vec![],
                    warnings: vec![],
                    hints: vec![],
                    data: None,
                    error: Some("push rejected".to_string()),
                }],
                status: ReleaseStepStatus::PartialSuccess,
                warnings: vec![],
                summary: None,
            },
        };

        assert_eq!(release_run_failure_exit(&run), 1);
    }

    #[test]
    fn release_summary_explicitly_reports_created_and_missing_release_surfaces() {
        let run = ReleaseRun {
            component_id: "demo".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![
                    ReleaseStepResult {
                        id: "git.commit".to_string(),
                        step_type: "git.commit".to_string(),
                        status: ReleaseStepStatus::Success,
                        missing: vec![],
                        warnings: vec![],
                        hints: vec![],
                        data: Some(serde_json::json!({ "success": true })),
                        error: None,
                    },
                    ReleaseStepResult {
                        id: "git.tag".to_string(),
                        step_type: "git.tag".to_string(),
                        status: ReleaseStepStatus::Success,
                        missing: vec![],
                        warnings: vec![],
                        hints: vec![],
                        data: Some(serde_json::json!({ "tag": "v1.2.3" })),
                        error: None,
                    },
                ],
                status: ReleaseStepStatus::Success,
                warnings: vec![],
                summary: None,
            },
        };

        let summary = release_summary_from_run(&run);

        assert!(summary.contains(&"Release created: v1.2.3".to_string()));
        assert!(summary.contains(&"Release commit created".to_string()));
        assert!(summary.contains(&"Tag created: v1.2.3".to_string()));
        assert!(summary.contains(&"No GitHub Release created".to_string()));
    }

    // ----- Recover-time orphan-tag warning (issue #2234 ask #3) -----

    fn run_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
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
        output
    }

    #[test]
    fn diagnose_orphan_tag_warns_when_tag_points_at_non_release_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_in(dir, &["git", "init", "-q"]);
        run_in(dir, &["git", "config", "user.email", "test@example.com"]);
        run_in(dir, &["git", "config", "user.name", "Test"]);
        run_in(dir, &["git", "config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("README"), "x").expect("write");
        run_in(dir, &["git", "add", "."]);
        run_in(
            dir,
            &["git", "commit", "-q", "-m", "Update h2bc bundle to v0.6.14"],
        );
        run_in(dir, &["git", "tag", "v0.7.6"]);

        let warning = diagnose_orphan_tag(&dir.to_string_lossy(), "v0.7.6")
            .expect("orphan tag should produce a warning");

        assert!(warning.contains("v0.7.6"));
        assert!(warning.contains("issue #2234"));
        assert!(warning.contains("Update h2bc bundle"));
    }

    #[test]
    fn diagnose_orphan_tag_silent_when_tag_points_at_release_commit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_in(dir, &["git", "init", "-q"]);
        run_in(dir, &["git", "config", "user.email", "test@example.com"]);
        run_in(dir, &["git", "config", "user.name", "Test"]);
        run_in(dir, &["git", "config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("README"), "x").expect("write");
        run_in(dir, &["git", "add", "."]);
        run_in(dir, &["git", "commit", "-q", "-m", "release: v0.7.4"]);
        run_in(dir, &["git", "tag", "v0.7.4"]);

        assert!(diagnose_orphan_tag(&dir.to_string_lossy(), "v0.7.4").is_none());
    }
}
