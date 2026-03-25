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

    fn commit(subject: &str, category: CommitCategory) -> CommitInfo {
        CommitInfo {
            hash: "abc1234".to_string(),
            subject: subject.to_string(),
            category,
        }
    }

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
    fn test_normalize_strips_pr_reference() {
        assert_eq!(
            normalize_changelog_text("fix something (#526)"),
            normalize_changelog_text("fix something")
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
    fn test_group_commits_strips_conventional_prefix_with_issue_scope() {
        use super::group_commits_for_changelog;

        let commits = vec![
            commit(
                "feat(#741): delete AgentType class — replace with string literals",
                CommitCategory::Feature,
            ),
            commit(
                "fix(#730): queue-add uses unified check-duplicate",
                CommitCategory::Fix,
            ),
        ];

        let entries = group_commits_for_changelog(&commits);
        let added = &entries["added"];
        let fixed = &entries["fixed"];

        assert_eq!(
            added[0],
            "delete AgentType class — replace with string literals"
        );
        assert_eq!(fixed[0], "queue-add uses unified check-duplicate");
    }

    #[test]
    fn test_group_commits_strips_pr_references() {
        use super::group_commits_for_changelog;

        let commits = vec![
            commit(
                "feat: agent-first scoping — Phase 1 schema (#738)",
                CommitCategory::Feature,
            ),
            commit(
                "fix: rename $class param — fixes bootstrap crash (#711)",
                CommitCategory::Fix,
            ),
        ];

        let entries = group_commits_for_changelog(&commits);
        let added = &entries["added"];
        let fixed = &entries["fixed"];

        assert_eq!(added[0], "agent-first scoping — Phase 1 schema");
        assert_eq!(fixed[0], "rename $class param — fixes bootstrap crash");
    }
}
