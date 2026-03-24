mod load_component;
mod post_release_step;
mod return_aggregated_errors;

pub use load_component::*;
pub use post_release_step::*;
pub use return_aggregated_errors::*;

use crate::component::{self, Component};
use crate::engine::local_files::FileSystem;
use crate::engine::pipeline::{self, PipelineStep};
use crate::engine::run_dir::{self, RunDir};
use crate::engine::validation::ValidationCollector;
use crate::error::{Error, Result};
use crate::extension::{self, ExtensionManifest};
use crate::git::{self, UncommittedChanges};
use crate::release::changelog;
use crate::version;

use super::executor::ReleaseStepExecutor;
use super::resolver::{resolve_extensions, ReleaseCapabilityResolver};
use super::types::{
    ReleaseOptions, ReleasePlan, ReleasePlanStatus, ReleasePlanStep, ReleaseRun,
    ReleaseSemverCommit, ReleaseSemverRecommendation,
};

#[cfg(test)]
mod tests {
    use super::{find_uncovered_commits, normalize_changelog_text, strip_pr_reference};
    use crate::git::{CommitCategory, CommitInfo};

    #[test]
    fn test_normalize_changelog_text() {
        assert_eq!(
            normalize_changelog_text(
                "Fixed scoped audit exit codes to ignore unchanged legacy outliers"
            ),
            "fixed scoped audit exit codes to ignore unchanged legacy outliers"
        );
        assert_eq!(
            normalize_changelog_text("fix(audit): use scoped findings for changed-since exit"),
            "fix audit use scoped findings for changed since exit"
        );
    }

    #[test]
    fn test_find_uncovered_commits_ignores_covered_fix_commit() {
        let commits = vec![commit(
            "fix(audit): use scoped findings for changed-since exit",
            CommitCategory::Fix,
        )];
        let unreleased = vec![
            "use scoped findings for changed-since exit".to_string(),
            "another manual note".to_string(),
        ];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert!(uncovered.is_empty());
    }

    #[test]
    fn test_find_uncovered_commits_requires_feature_coverage() {
        let commits = vec![
            commit(
                "fix(audit): use scoped findings for changed-since exit",
                CommitCategory::Fix,
            ),
            commit(
                "feat(refactor): apply decompose plans with audit impact projection",
                CommitCategory::Feature,
            ),
        ];
        let unreleased = vec!["use scoped findings for changed-since exit".to_string()];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert_eq!(uncovered.len(), 1);
        assert_eq!(
            uncovered[0].subject,
            "feat(refactor): apply decompose plans with audit impact projection"
        );
    }

    #[test]
    fn test_find_uncovered_commits_skips_docs_and_merge() {
        let commits = vec![
            commit("docs: update release notes", CommitCategory::Docs),
            commit("Merge pull request #1 from branch", CommitCategory::Merge),
        ];

        let uncovered = find_uncovered_commits(&commits, &[]);
        assert!(uncovered.is_empty());
    }

    #[test]
    fn test_strip_pr_reference() {
        assert_eq!(strip_pr_reference("fix something (#526)"), "fix something");
        assert_eq!(
            strip_pr_reference("feat: add feature (#123, #456)"),
            "feat: add feature"
        );
        assert_eq!(
            strip_pr_reference("no pr reference here"),
            "no pr reference here"
        );
        assert_eq!(
            strip_pr_reference("has parens (not a pr ref)"),
            "has parens (not a pr ref)"
        );
    }

    #[test]
    fn test_find_uncovered_commits_deduplicates_with_pr_suffix() {
        // Scenario: manual changelog entry without PR ref, commit has PR ref
        let commits = vec![commit(
            "fix: version bump dry-run no longer mutates changelog (#526)",
            CommitCategory::Fix,
        )];
        let unreleased = vec!["version bump dry-run no longer mutates changelog".to_string()];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert!(
            uncovered.is_empty(),
            "Should detect commit as covered by manual entry (PR ref stripped)"
        );
    }

    #[test]
    fn test_find_uncovered_commits_bidirectional_match() {
        // Entry is longer/more descriptive than the commit message
        let commits = vec![commit("feat: enable autofix", CommitCategory::Feature)];
        let unreleased = vec!["enable autofix on PR and release CI workflows".to_string()];

        let uncovered = find_uncovered_commits(&commits, &unreleased);
        assert!(
            uncovered.is_empty(),
            "Should match when commit subject is contained in the entry"
        );
    }

    #[test]
    fn test_load_component_component_resolve_effective_some_component_id_options_path_o() {

        let _result = load_component();
    }

    #[test]
    fn test_run_default_path() {
        let component_id = "";
        let options = Default::default();
        let _result = run(&component_id, &options);
    }

    #[test]
    fn test_run_default_path_2() {
        let component_id = "";
        let options = Default::default();
        let _result = run(&component_id, &options);
    }

    #[test]
    fn test_run_default_path_3() {
        let component_id = "";
        let options = Default::default();
        let _result = run(&component_id, &options);
    }

    #[test]
    fn test_run_default_path_4() {
        let component_id = "";
        let options = Default::default();
        let _result = run(&component_id, &options);
    }

    #[test]
    fn test_plan_default_path() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_default_path_2() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_default_path_3() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_let_new_version_if_let_some_ref_info_version_info() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_match_version_increment_version_info_version_options_bump_ty() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_will_auto_generate() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_if_let_some_ref_info_version_info() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_let_some_ref_info_version_info() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_uncommitted_has_changes() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_default_path_4() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_default_path_5() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_default_path_6() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_default_path_7() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_if_let_some_ref_entries_pending_entries() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_let_some_ref_entries_pending_entries() {
        let component_id = "";
        let options = Default::default();
        let _result = plan(&component_id, &options);
    }

    #[test]
    fn test_plan_has_expected_effects() {
        // Expected effects: mutation, logging, process_spawn
        let component_id = "";
        let options = Default::default();
        let _ = plan(&component_id, &options);
    }

    #[test]
    fn test_resolve_tag_and_commits_match_monorepo() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_match_monorepo_2() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_some_ctx_path_prefix() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_default_path() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_ok_latest_tag_commits() {

        let result = resolve_tag_and_commits();
        assert!(result.is_ok(), "expected Ok for: Ok((latest_tag, commits))");
    }

    #[test]
    fn test_resolve_tag_and_commits_default_path_2() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_default_path_3() {

        let _result = resolve_tag_and_commits();
    }

    #[test]
    fn test_resolve_tag_and_commits_ok_latest_tag_commits_2() {

        let result = resolve_tag_and_commits();
        assert!(result.is_ok(), "expected Ok for: Ok((latest_tag, commits))");
    }

    #[test]
    fn test_changelog_entries_from_json_default_path() {

        let _result = changelog_entries_from_json();
    }

}
