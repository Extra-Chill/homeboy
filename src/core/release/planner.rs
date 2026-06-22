//! Release planning: validate inputs and build the executable release plan.

use crate::core::engine::validation::ValidationCollector;
use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::release::version;

use super::context::{load_component, resolve_extensions};
use super::plan_steps::{build_preflight_steps, build_release_steps};
use super::planning_changelog::{build_changelog_plan, generate_changelog_entries};
use super::planning_policy::release_skip_plan;
use super::planning_semver::{
    build_semver_recommendation, current_version_tag_at_head, current_version_tag_name,
    validate_current_version_tag_reachable, validate_release_version_floor,
};
use super::planning_worktree::validate_release_worktree;
use super::types::{
    ReleaseChangelogPlan, ReleaseOptions, ReleasePlan, ReleaseSemverRecommendation,
};

const OVERSIZED_PATCH_RELEASE_ITEM_THRESHOLD: usize = 50;

/// Plan a release: run all preflight validations, then return a description
/// of the steps the executor will run. Used by `--dry-run` to preview work
/// without side effects and by release execution to drive the same steps.
///
/// Requires a clean working tree (uncommitted changes cause an error).
pub fn plan(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
    let component = load_component(component_id, options)?;
    let extensions = resolve_extensions(&component)?;

    let mut v = ValidationCollector::new();

    let monorepo = git::MonorepoContext::detect(&component.local_path, component_id);
    let version_info = v.capture(version::read_component_version(&component), "version");
    if let Some(ref info) = version_info {
        if let Some(message) = v
            .capture(
                validate_current_version_tag_reachable(
                    &component.local_path,
                    monorepo.as_ref(),
                    &info.version,
                ),
                "tag",
            )
            .flatten()
        {
            let tag_name = current_version_tag_name(monorepo.as_ref(), &info.version);
            v.push(
                "tag",
                &message,
                Some(serde_json::json!({
                    "version": &info.version,
                    "tag": &tag_name,
                    "recovery": [
                        format!("Inspect the existing tag: git show --no-patch --decorate {}", tag_name),
                        format!("If the orphaned tag is abandoned, delete it locally and remotely: git tag -d {} && git push origin :refs/tags/{}", tag_name, tag_name),
                        format!("Then rerun recovery: homeboy release {} --recover", component_id),
                        format!("If the tag is valid, check out or merge the tagged release commit before releasing {}", component_id),
                    ]
                })),
            );
        }
    }
    let mut semver_recommendation = if options.pipeline.head {
        None
    } else {
        build_semver_recommendation(&component, &options.bump_type, monorepo.as_ref())?
    };

    if !options.pipeline.head {
        // Catch "release vX.Y.Z already exists at HEAD" before the bump/semver
        // gate so a forced re-run after a prior partial release sees a clear
        // skip plan instead of a downstream changelog contract error for the
        // next version (issue #4316).
        let release_already_at_head = version_info.as_ref().and_then(|info| {
            current_version_tag_at_head(&component.local_path, monorepo.as_ref(), &info.version)
                .ok()
                .flatten()
        });

        if let Some(skip_plan) = release_skip_plan(
            component_id,
            options,
            semver_recommendation.clone(),
            release_already_at_head.as_deref(),
        ) {
            return Ok(skip_plan);
        }
    }

    let pending_entries = if options.pipeline.head {
        Default::default()
    } else {
        v.capture(
            generate_changelog_entries(&component, component_id, options, monorepo.as_ref()),
            "commits",
        )
        .unwrap_or_default()
    };

    let new_version = if let Some(ref info) = version_info {
        if options.pipeline.head {
            Some(info.version.clone())
        } else {
            match version::increment_version(&info.version, &options.bump_type) {
                Some(ver) => Some(ver),
                None => {
                    v.push(
                        "version",
                        &format!("Invalid version format: {}", info.version),
                        None,
                    );
                    None
                }
            }
        }
    } else {
        None
    };

    if let (Some(ref info), Some(ref next_version)) = (&version_info, &new_version) {
        if let Some(message) = validate_release_version_floor(
            semver_recommendation
                .as_ref()
                .and_then(|rec| rec.latest_tag.as_deref()),
            &info.version,
            next_version,
        ) {
            v.push("version", &message, None);
        }
    }

    if let Some(ref info) = version_info {
        if let Some(details) = validate_release_worktree(&component, options, info)? {
            v.push(
                "working_tree",
                "Uncommitted changes detected",
                Some(details),
            );
        }
    }

    v.finish()?;

    let version_info = version_info.ok_or_else(|| {
        Error::internal_unexpected("version_info missing after validation".to_string())
    })?;
    let new_version = new_version.ok_or_else(|| {
        Error::internal_unexpected("new_version missing after validation".to_string())
    })?;

    let mut warnings = Vec::new();
    let mut hints = Vec::new();
    let changelog_plan = build_changelog_plan(&component, options, pending_entries)?;
    if let Some(warning) =
        apply_oversized_patch_release_policy(&mut semver_recommendation, &changelog_plan)
    {
        warnings.push(warning);
    }

    let mut steps = build_preflight_steps(options, semver_recommendation.as_ref(), &extensions);
    steps.extend(build_release_steps(
        &component,
        &extensions,
        &version_info.version,
        &new_version,
        &changelog_plan,
        options,
        monorepo.as_ref(),
        &mut warnings,
        &mut hints,
    )?);

    if options.dry_run {
        hints.push("Dry run: no changes will be made".to_string());
    }

    Ok(ReleasePlan::new(
        component_id,
        true,
        steps,
        semver_recommendation,
        warnings,
        hints,
    ))
}

fn apply_oversized_patch_release_policy(
    semver_recommendation: &mut Option<ReleaseSemverRecommendation>,
    changelog_plan: &ReleaseChangelogPlan,
) -> Option<String> {
    let semver_recommendation = semver_recommendation.as_mut()?;
    if semver_recommendation.requested_bump != "patch" {
        return None;
    }
    if semver_recommendation.recommended_bump.as_deref() != Some("patch") {
        return None;
    }

    let commit_count = semver_recommendation.commits.len();
    let changelog_entry_count = changelog_plan.entry_count;
    if commit_count < OVERSIZED_PATCH_RELEASE_ITEM_THRESHOLD
        && changelog_entry_count < OVERSIZED_PATCH_RELEASE_ITEM_THRESHOLD
    {
        return None;
    }

    semver_recommendation.recommended_bump = Some("minor".to_string());
    semver_recommendation.is_underbump = true;
    semver_recommendation.reasons.push(format!(
        "release range has {} commits and {} changelog entries; release-train-sized patch ranges require a minor bump",
        commit_count, changelog_entry_count
    ));

    Some(format!(
        "Patch release range is large ({} commits, {} changelog entries). Consider `--bump minor` for release-train-sized changes, or confirm the patch scope before releasing.",
        commit_count, changelog_entry_count
    ))
}

#[cfg(test)]
mod tests {
    use super::{apply_oversized_patch_release_policy, plan};
    use crate::core::release::types::{
        ReleaseChangelogPlan, ReleaseOptions, ReleaseSemverCommit, ReleaseSemverRecommendation,
    };
    use std::collections::HashMap;

    #[test]
    fn test_plan() {
        let err = plan(
            "missing-component-is-reported-by-planner",
            &ReleaseOptions::default(),
        )
        .expect_err("planner should report missing components");

        assert!(!err.to_string().is_empty());
    }

    /// Regression for the homeboy-action release blocker:
    /// `validate_working_tree_fail_fast` builds an Error with a hint vec
    /// listing the dirty files. That error flows through ValidationCollector,
    /// which used to drop the hints on the single-error re-emit path —
    /// leaving CI consumers with a bare `Uncommitted changes detected`
    /// message and no way to see *which* files were dirty.
    ///
    /// This test pins down the round-trip: build the same shape of error
    /// that `validate_working_tree_fail_fast` would produce, push it through
    /// `ValidationCollector::finish_if_errors`, and assert the dirty file
    /// hints survive in the resulting JSON details.
    #[test]
    fn working_tree_fail_fast_error_preserves_file_hints_through_collector() {
        use crate::core::engine::validation::ValidationCollector;
        use crate::core::error::Error;

        let original = Error::validation_invalid_argument(
            "working_tree",
            "Uncommitted changes detected — refusing to release",
            None,
            Some(vec![
                "Commit, stash, or discard changes before releasing".to_string(),
                "Unexpected dirty files (2): src/lib.rs, Cargo.lock".to_string(),
            ]),
        );

        let mut collector = ValidationCollector::new();
        collector.capture::<()>(Err(original), "working_tree");
        let propagated = collector.finish_if_errors().unwrap_err();

        let details = &propagated.details;
        let tried = details
            .get("tried")
            .and_then(|v| v.as_array())
            .expect("tried hints must survive collector round-trip");
        assert_eq!(tried.len(), 2, "expected both hints to survive: {details}");
        let joined: String = tried
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            joined.contains("src/lib.rs"),
            "dirty file list must reach the JSON envelope, got: {joined}"
        );
        assert!(
            joined.contains("Cargo.lock"),
            "dirty file list must reach the JSON envelope, got: {joined}"
        );
    }

    #[test]
    fn oversized_patch_release_warning_reports_large_patch_scope() {
        let mut recommendation = Some(semver_recommendation("patch", 206));
        let warning =
            apply_oversized_patch_release_policy(&mut recommendation, &changelog_plan(206))
                .expect("large patch release should warn");
        let recommendation = recommendation.expect("recommendation remains available");

        assert!(warning.contains("Patch release range is large"));
        assert!(warning.contains("206 commits"));
        assert!(warning.contains("206 changelog entries"));
        assert!(warning.contains("--bump minor"));
        assert_eq!(recommendation.recommended_bump.as_deref(), Some("minor"));
        assert!(recommendation.is_underbump);
        assert!(recommendation.reasons.iter().any(|reason| {
            reason.contains("release-train-sized patch ranges require a minor bump")
        }));
    }

    #[test]
    fn oversized_patch_release_warning_is_quiet_for_small_patch_scope() {
        let mut recommendation = Some(semver_recommendation("patch", 3));
        let warning = apply_oversized_patch_release_policy(&mut recommendation, &changelog_plan(3));

        assert!(warning.is_none());
        let recommendation = recommendation.expect("recommendation remains available");
        assert_eq!(recommendation.recommended_bump.as_deref(), Some("patch"));
        assert!(!recommendation.is_underbump);
    }

    #[test]
    fn oversized_patch_release_policy_requires_minor_when_commit_range_is_large() {
        let mut recommendation = Some(semver_recommendation("patch", 61));
        let warning = apply_oversized_patch_release_policy(&mut recommendation, &changelog_plan(1));

        assert!(warning.is_some());
        let recommendation = recommendation.expect("recommendation remains available");
        assert_eq!(recommendation.recommended_bump.as_deref(), Some("minor"));
        assert!(recommendation.is_underbump);
    }

    #[test]
    fn oversized_patch_release_policy_requires_minor_when_changelog_range_is_large() {
        let mut recommendation = Some(semver_recommendation("patch", 1));
        let warning =
            apply_oversized_patch_release_policy(&mut recommendation, &changelog_plan(61));

        assert!(warning.is_some());
        let recommendation = recommendation.expect("recommendation remains available");
        assert_eq!(recommendation.recommended_bump.as_deref(), Some("minor"));
        assert!(recommendation.is_underbump);
    }

    fn semver_recommendation(
        requested_bump: &str,
        commit_count: usize,
    ) -> ReleaseSemverRecommendation {
        ReleaseSemverRecommendation {
            latest_tag: Some("v1.2.3".to_string()),
            range: "v1.2.3..HEAD".to_string(),
            commits: (0..commit_count)
                .map(|index| ReleaseSemverCommit {
                    sha: format!("{index:08x}"),
                    subject: format!("fix: change {index}"),
                    commit_type: "fix".to_string(),
                    breaking: false,
                })
                .collect(),
            recommended_bump: Some("patch".to_string()),
            requested_bump: requested_bump.to_string(),
            is_underbump: false,
            reasons: Vec::new(),
        }
    }

    fn changelog_plan(entry_count: usize) -> ReleaseChangelogPlan {
        let mut entries = HashMap::new();
        entries.insert(
            "fixed".to_string(),
            (0..entry_count)
                .map(|index| format!("change {index}"))
                .collect(),
        );

        ReleaseChangelogPlan {
            policy: "generated".to_string(),
            path: "CHANGELOG.md".to_string(),
            dry_run: true,
            entries,
            entry_count,
        }
    }
}
