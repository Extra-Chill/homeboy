use super::context::load_component;
use super::execution_dispatch::{execute_release_plan_step, ReleaseExecutionContext};
use super::types::{ReleaseOptions, ReleaseState, ReleaseStepStatus};
use std::path::Path;

fn git(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fixture(locked: &str, newest: &str) -> (tempfile::TempDir, ReleaseOptions) {
    let temp = tempfile::tempdir().unwrap();
    let upstream = temp.path().join("upstream");
    let downstream = temp.path().join("downstream");
    std::fs::create_dir_all(&upstream).unwrap();
    std::fs::create_dir_all(&downstream).unwrap();
    git(&upstream, &["init", "-q", "-b", "main"]);
    std::fs::write(upstream.join("README"), "upstream").unwrap();
    git(&upstream, &["add", "."]);
    git(&upstream, &["commit", "-qm", "fix: upstream"]);
    git(&upstream, &["tag", &format!("engine-v{locked}")]);
    if newest != locked {
        git(&upstream, &["tag", &format!("engine-v{newest}")]);
    }
    let config = serde_json::json!({
        "id": "downstream", "type": "fixture", "release": {"refresh_dependencies": true},
        "version_targets": [{"file": "VERSION", "pattern": "([0-9]+\\.[0-9]+\\.[0-9]+)"}],
        "changelog_target": "CHANGELOG.md",
        "deps": {"provider": "fixture", "packages": [{"name": "fixture/engine", "manifest_section": "dependencies", "constraint": locked, "locked_version": locked}]},
        "dependency_stack": [{"upstream": "engine", "downstream": "downstream", "package": "fixture/engine", "source": {"repo": upstream, "prefix": "engine"},
            "update": format!("printf '{newest}\\n' > deps.lock; printf '{newest}\\n' > deps.manifest") }]
    });
    std::fs::write(
        downstream.join("homeboy.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    std::fs::write(downstream.join("VERSION"), "0.1.0\n").unwrap();
    std::fs::write(downstream.join("deps.lock"), locked).unwrap();
    std::fs::write(downstream.join("deps.manifest"), locked).unwrap();
    std::fs::write(
        downstream.join("CHANGELOG.md"),
        "# Changelog\n\n## [0.1.0] - 2026-01-01\n\n### Changed\n- Initial release\n",
    )
    .unwrap();
    git(&downstream, &["init", "-q", "-b", "main"]);
    git(&downstream, &["add", "."]);
    git(&downstream, &["commit", "-qm", "release: v0.1.0"]);
    git(&downstream, &["tag", "v0.1.0"]);
    let options = ReleaseOptions {
        path_override: Some(downstream.display().to_string()),
        bump_type: "none".to_string(),
        dry_run: true,
        ..Default::default()
    };
    (temp, options)
}

#[test]
fn dependency_refresh_only_release_commits_manifest_lockfile_and_changelog() {
    let (temp, options) = fixture("1.0.0", "1.0.1");
    let path = temp.path().join("downstream");
    let plan = super::planner::plan("downstream", &options).expect("refresh-only release plans");
    assert!(plan.enabled());
    assert!(plan
        .plan
        .steps
        .iter()
        .any(|s| s.kind == "deps.stack.refresh"));
    // Planning is a preview even for a real release.
    assert!(git(&path, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        std::fs::read_to_string(path.join("deps.lock")).unwrap(),
        "1.0.0"
    );
    let real_options = ReleaseOptions {
        dry_run: false,
        ..options
    };
    let real_plan = super::planner::plan("downstream", &real_options).unwrap();
    assert!(git(&path, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        plan.plan
            .steps
            .iter()
            .find(|s| s.kind == "deps.stack.refresh")
            .unwrap()
            .inputs,
        real_plan
            .plan
            .steps
            .iter()
            .find(|s| s.kind == "deps.stack.refresh")
            .unwrap()
            .inputs
    );
    let component = load_component("downstream", &real_options).unwrap();
    let mut context = ReleaseExecutionContext {
        component: &component,
        component_id: "downstream",
        options: &real_options,
        extensions: &[],
        roots: homeboy_core::paths::PathRoots::new(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("artifacts"),
        ),
        state: ReleaseState::default(),
        publish_failed: false,
    };
    for step in &plan.plan.steps {
        if matches!(
            step.kind.as_str(),
            "deps.stack.refresh" | "changelog.finalize" | "version" | "git.commit"
        ) {
            let result = execute_release_plan_step(step, &mut context)
                .unwrap()
                .unwrap();
            assert_eq!(result.status, ReleaseStepStatus::Success, "{result:?}");
        }
    }
    let committed = git(&path, &["show", "--format=", "--name-only", "HEAD"]);
    for file in ["deps.lock", "deps.manifest", "CHANGELOG.md", "VERSION"] {
        assert!(committed.lines().any(|line| line == file), "{committed}");
    }
    assert!(git(&path, &["show", "HEAD:CHANGELOG.md"]).contains("Update fixture/engine to 1.0.1"));
    assert!(git(&path, &["status", "--porcelain"]).is_empty());
}

#[test]
fn dependency_refresh_rejects_upstream_drift_before_mutating() {
    let (temp, options) = fixture("1.0.0", "1.0.1");
    let component = load_component("downstream", &options).unwrap();
    let preview = super::dependency_refresh::preview(&component).unwrap();
    git(&temp.path().join("upstream"), &["tag", "engine-v1.0.2"]);
    let error =
        super::dependency_refresh::apply(&component, &serde_json::to_value(preview).unwrap())
            .unwrap_err();
    assert!(error.message.contains("changed since planning"));
    assert!(git(&temp.path().join("downstream"), &["status", "--porcelain"]).is_empty());
}

#[test]
fn dependency_refresh_current_leaves_plan_unchanged() {
    let (temp, options) = fixture("1.0.0", "1.0.0");
    let with = super::planner::plan("downstream", &options).unwrap();
    let path = temp.path().join("downstream");
    let config_path = path.join("homeboy.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    config["release"]["refresh_dependencies"] = false.into();
    std::fs::write(config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    let without = super::planner::plan("downstream", &options).unwrap();
    assert_eq!(
        serde_json::to_value(with).unwrap(),
        serde_json::to_value(without).unwrap()
    );
}

#[test]
fn dependency_refresh_breaking_and_unknown_block_without_mutation() {
    for newest in ["2.0.0", "1.0.1"] {
        let (temp, options) = fixture("1.0.0", newest);
        if newest == "1.0.1" {
            git(
                &temp.path().join("upstream"),
                &["tag", "-d", "engine-v1.0.0"],
            );
        }
        let error = super::planner::plan("downstream", &options).unwrap_err();
        assert!(
            error.message.contains("breaking or unknown"),
            "{}",
            error.message
        );
        assert!(git(&temp.path().join("downstream"), &["status", "--porcelain"]).is_empty());
    }
}
