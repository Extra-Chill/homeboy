use crate::stack::candidates::{preflight, stale_stack_error};
use crate::stack::{
    discover_candidates, save, GitRef, StackPrEntry, StackProvenance, StackRequirements, StackSpec,
};
use homeboy_core::test_support::with_isolated_home;
use std::fs;
use std::process::Command;

mod support;
use support::{commit_file, git, init_repo, rev_parse};

/// The isolated home each test below installs, named as a config root.
///
/// A test is the entry point for its own unit of work, so resolving once here is
/// a boundary resolution, not an ambient one. What matters is that the
/// production path beneath it resolves nothing (#7505).
fn test_config_root() -> std::path::PathBuf {
    homeboy_core::paths::PathRoots::from_environment()
        .expect("path roots")
        .config()
        .to_path_buf()
}

fn stack(id: &str, component: &str, base: &str, prs: &[(&str, u64)]) -> StackSpec {
    StackSpec {
        id: id.to_string(),
        description: String::new(),
        component: component.to_string(),
        component_path: "/tmp/component".to_string(),
        base: GitRef {
            remote: "origin".to_string(),
            branch: base.to_string(),
        },
        target: GitRef {
            remote: "fork".to_string(),
            branch: format!("stack/{id}"),
        },
        prs: prs
            .iter()
            .map(|(repo, number)| StackPrEntry {
                repo: (*repo).to_string(),
                number: *number,
                note: None,
            })
            .collect(),
        provenance: None,
        requirements: StackRequirements::default(),
    }
}

#[test]
fn stale_trunk_stack_ranks_compatible_pr_based_candidate_first() {
    with_isolated_home(|_| {
        let stale = stack(
            "trunk",
            "playground",
            "trunk",
            &[("Automattic/playground", 10)],
        );
        let mut candidate = stack(
            "abi-pr",
            "playground",
            "abi-pr",
            &[("Automattic/playground", 10), ("Automattic/playground", 20)],
        );
        candidate.provenance = Some(StackProvenance {
            source: "https://github.com/Automattic/playground-stacks".to_string(),
            revision: Some("abc123".to_string()),
        });
        candidate
            .requirements
            .compatible_bases
            .push(stale.base.clone());
        save(&test_config_root(), &stale).expect("save stale stack");
        save(&test_config_root(), &candidate).expect("save candidate stack");

        let candidates =
            discover_candidates(&test_config_root(), &stale).expect("discover candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].stack_id, "abi-pr");
        assert!(candidates[0].base_compatible);
        assert_eq!(candidates[0].pr_overlap.len(), 1);
        assert_eq!(
            candidates[0]
                .provenance
                .as_ref()
                .unwrap()
                .revision
                .as_deref(),
            Some("abc123")
        );
        assert_eq!(
            candidates[0].selection_command,
            "homeboy stack apply abi-pr"
        );
    });
}

#[test]
fn ambiguous_candidates_are_ranked_by_overlap_then_id() {
    with_isolated_home(|_| {
        let source = stack(
            "source",
            "playground",
            "trunk",
            &[("Automattic/playground", 10), ("Automattic/playground", 20)],
        );
        let mut alpha = stack(
            "alpha",
            "playground",
            "topic",
            &[("Automattic/playground", 10)],
        );
        alpha
            .requirements
            .compatible_bases
            .push(source.base.clone());
        let mut beta = stack(
            "beta",
            "playground",
            "topic",
            &[("Automattic/playground", 10)],
        );
        beta.requirements.compatible_bases.push(source.base.clone());
        let mut overlap = stack(
            "overlap",
            "playground",
            "topic",
            &[("Automattic/playground", 10), ("Automattic/playground", 20)],
        );
        overlap
            .requirements
            .compatible_bases
            .push(source.base.clone());
        save(&test_config_root(), &source).unwrap();
        save(&test_config_root(), &beta).unwrap();
        save(&test_config_root(), &overlap).unwrap();
        save(&test_config_root(), &alpha).unwrap();

        let candidates = discover_candidates(&test_config_root(), &source).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.stack_id.as_str())
                .collect::<Vec<_>>(),
            ["overlap", "alpha", "beta"]
        );
    });
}

#[test]
fn stale_target_with_compatible_candidate_is_blocked_without_mutation() {
    with_isolated_home(|_| {
        let (dir, path) = init_repo();
        commit_file(&dir, &path, "base.txt", "fresh base\n", "advance base");
        let base_sha = rev_parse(&path, "main");
        git(&path, &["checkout", "-q", "-b", "stack/source", "HEAD~1"]);
        let target_sha = commit_file(&dir, &path, "target.txt", "keep me\n", "old target");
        git(&path, &["checkout", "-q", "main"]);

        let mut source = stack(
            "source",
            "playground",
            "trunk",
            &[("Automattic/playground", 10)],
        );
        source.component_path = path.clone();
        source.base = GitRef {
            remote: "origin".to_string(),
            branch: "main".to_string(),
        };
        source.target.branch = "stack/source".to_string();
        let mut alternative = stack(
            "topic-stack",
            "playground",
            "topic",
            &[("Automattic/playground", 10)],
        );
        alternative
            .requirements
            .compatible_bases
            .push(source.base.clone());
        save(&test_config_root(), &source).unwrap();
        save(&test_config_root(), &alternative).unwrap();

        let report = preflight(&test_config_root(), &source, &path, "main").expect("preflight");
        assert!(report.blocked);
        assert_eq!(report.target_behind, Some(1));
        assert_eq!(report.candidates[0].stack_id, "topic-stack");

        let error = stale_stack_error(&source, &report);
        assert_eq!(error.details["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(error.details["candidates"][0]["stack_id"], "topic-stack");
        assert_eq!(rev_parse(&path, "stack/source"), target_sha);
        assert_eq!(rev_parse(&path, "main"), base_sha);
        assert!(fs::metadata(dir.path().join("target.txt")).is_err());
        let status = Command::new("git")
            .args(["status", "--porcelain=v1"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(status.stdout.is_empty());
    });
}

#[test]
fn unrelated_stacks_are_excluded_and_missing_provenance_is_null() {
    with_isolated_home(|_| {
        let source = stack(
            "source",
            "playground",
            "trunk",
            &[("Automattic/playground", 10)],
        );
        let matching = stack(
            "matching",
            "playground",
            "topic",
            &[("Automattic/playground", 99)],
        );
        let other_component = stack(
            "other-component",
            "wordpress",
            "topic",
            &[("Automattic/playground", 10)],
        );
        let other_repository = stack(
            "other-repository",
            "playground",
            "topic",
            &[("Automattic/wordpress", 10)],
        );
        save(&test_config_root(), &source).unwrap();
        save(&test_config_root(), &matching).unwrap();
        save(&test_config_root(), &other_component).unwrap();
        save(&test_config_root(), &other_repository).unwrap();

        let candidates = discover_candidates(&test_config_root(), &source).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].stack_id, "matching");
        assert!(candidates[0].provenance.is_none());
        let json = serde_json::to_value(&candidates[0]).unwrap();
        assert!(json["provenance"].is_null());
    });
}
