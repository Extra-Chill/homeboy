//! Release planning: validate inputs and build the executable release plan.

use crate::core::engine::validation::ValidationCollector;
use crate::core::error::{Error, Result};
use crate::core::release::version;

use super::context::{load_component, resolve_extensions};
use super::plan_steps::{build_preflight_steps, build_release_steps};
use super::planning_changelog::{build_changelog_plan, generate_changelog_entries};
use super::planning_policy::release_skip_plan;
use super::planning_semver::{
    build_semver_recommendation, current_version_tag_at_head, current_version_tag_name,
    release_version_floor_base, validate_current_version_tag_reachable,
    validate_release_version_floor,
};
use super::planning_worktree::validate_release_worktree;
use super::scope::ReleaseScope;
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
    let mut warnings = Vec::new();

    let release_scope = ReleaseScope::resolve(&component, component_id)?;
    let version_info = v.capture(version::read_component_version(&component), "version");
    if let Some(ref info) = version_info {
        if let Some(message) = v
            .capture(
                validate_current_version_tag_reachable(&release_scope, &info.version),
                "tag",
            )
            .flatten()
        {
            let tag_name = current_version_tag_name(&release_scope, &info.version);
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
        build_semver_recommendation(&component, &options.bump_type, &release_scope)?
    };

    if !options.pipeline.head {
        // Catch "release vX.Y.Z already exists at HEAD" before the bump/semver
        // gate so a forced re-run after a prior partial release sees a clear
        // skip plan instead of a downstream changelog contract error for the
        // next version (issue #4316).
        let release_already_at_head = version_info.as_ref().and_then(|info| {
            current_version_tag_at_head(&release_scope, &info.version)
                .ok()
                .flatten()
        });

        // A stale checkout whose HEAD still sits at the latest tag while its
        // upstream is ahead would report `release-already-at-head` and silently
        // skip genuinely-releasable work. Fail closed with an actionable refresh
        // hint instead of skipping. (#7945 / #7435)
        if let Some(ref tag) = release_already_at_head {
            guard_stale_primary_at_head(component_id, &release_scope.git_root, tag)?;
        }

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
            generate_changelog_entries(&component, component_id, options, &release_scope),
            "commits",
        )
        .unwrap_or_default()
    };

    let new_version = if let Some(ref info) = version_info {
        if options.pipeline.head {
            Some(info.version.clone())
        } else {
            let (version_floor_base, floor_tag) = v
                .capture(
                    release_version_floor_base(&release_scope, &info.version),
                    "tag",
                )
                .unwrap_or_else(|| (info.version.clone(), None));
            if let Some(tag) = floor_tag {
                warnings.push(format!(
                    "Latest release tag {} is ahead of source version {}; planning the next release from {} to avoid reusing an existing tag.",
                    tag, info.version, version_floor_base
                ));
            }
            match version::increment_version(&version_floor_base, &options.bump_type) {
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
        &release_scope,
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

/// Fail closed when a checkout reports `release-already-at-head` only because it
/// is stale — HEAD sits at the latest tag while its upstream is ahead.
///
/// Without this guard, `homeboy release` plans against a registered primary that
/// is behind `origin/<branch>` and silently skips (or reports `bump: none`),
/// hiding genuinely-releasable merged work. The check uses **local** tracking
/// refs (no network fetch), so it never slows planning or breaks offline and
/// materialized-checkout flows; it only fires when git already knows the
/// checkout is behind. (#7945 / #7435)
fn guard_stale_primary_at_head(component_id: &str, git_root: &str, tag: &str) -> Result<()> {
    let Ok(snapshot) = crate::core::git::get_repo_snapshot(git_root) else {
        return Ok(());
    };

    let behind = snapshot.behind.unwrap_or(0);
    if behind == 0 {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release-stale-checkout",
        format!(
            "Refusing to skip `{component_id}` as release-already-at-head: the checkout HEAD is at tag {tag} but its upstream ({branch}) is {behind} commit(s) ahead. Planning against this stale checkout would hide releasable merged work.",
            branch = snapshot.branch,
        ),
        None,
        Some(vec![
            format!(
                "Refresh the checkout to its upstream, then rerun: git -C {git_root} pull --ff-only && homeboy release {component_id}"
            ),
            "If you intend to release exactly this commit, check out the ref you want to release before planning.".to_string(),
        ]),
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
    use super::apply_oversized_patch_release_policy;
    use crate::core::release::types::{
        ReleaseChangelogPlan, ReleaseSemverCommit, ReleaseSemverRecommendation,
    };
    use std::collections::HashMap;

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

    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command runs");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Build a local clone whose HEAD is at `v1.0.0` and an "origin" that is
    /// `extra_upstream_commits` ahead, then update tracking refs without
    /// checking out. Returns the local clone path.
    fn stale_checkout_fixture(root: &Path, extra_upstream_commits: usize) -> std::path::PathBuf {
        let origin = root.join("origin");
        let local = root.join("local");
        std::fs::create_dir_all(&origin).expect("origin dir");

        git(&origin, &["init", "-b", "main"]);
        git(&origin, &["config", "user.email", "t@example.com"]);
        git(&origin, &["config", "user.name", "T"]);
        std::fs::write(origin.join("f.txt"), "0\n").expect("seed file");
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "release: v1.0.0"]);
        git(&origin, &["tag", "v1.0.0"]);

        git(
            &origin,
            &["clone", ".", local.to_str().expect("local path")],
        );
        git(&local, &["config", "user.email", "t@example.com"]);
        git(&local, &["config", "user.name", "T"]);

        for index in 0..extra_upstream_commits {
            std::fs::write(origin.join("f.txt"), format!("{}\n", index + 1)).expect("write");
            git(&origin, &["add", "."]);
            git(&origin, &["commit", "-m", &format!("feat: change {index}")]);
        }

        // Update tracking refs locally (no working-tree move): the local clone
        // now knows origin/main is ahead while HEAD stays at v1.0.0.
        git(&local, &["fetch", "origin"]);
        local
    }

    #[test]
    fn guard_fails_closed_when_checkout_is_behind_upstream() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let local = stale_checkout_fixture(dir.path(), 2);

        let result = guard_stale_primary_at_head("demo", local.to_str().expect("path"), "v1.0.0");
        let error = result.expect_err("behind checkout must fail closed");
        assert!(
            error.message.contains("stale") || error.message.contains("ahead"),
            "error should explain the stale checkout, got: {}",
            error.message
        );
    }

    #[test]
    fn guard_allows_checkout_at_upstream_head() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        // Zero extra upstream commits: HEAD == origin/main, genuinely at head.
        let local = stale_checkout_fixture(dir.path(), 0);

        assert!(
            guard_stale_primary_at_head("demo", local.to_str().expect("path"), "v1.0.0").is_ok(),
            "an up-to-date checkout at the tag must still skip cleanly"
        );
    }
}
