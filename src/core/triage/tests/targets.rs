use super::super::*;
use super::*;

#[test]
fn resolve_repo_prefers_triage_remote_without_losing_source_repo() {
    let component_ref = ComponentRef::new(
        "playground".to_string(),
        "/tmp/playground".to_string(),
        Some("https://github.com/example-org/wordpress-playground.git".to_string()),
        Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
        "component:playground".to_string(),
    );

    let resolved = resolve_repo(&component_ref).unwrap();

    assert_eq!(resolved.repo.owner, "WordPress");
    assert_eq!(resolved.repo.repo, "wordpress-playground");
    assert_eq!(
        resolved.triage_remote_url.as_deref(),
        Some("https://github.com/WordPress/wordpress-playground.git")
    );
    let source = resolved.source_repo.expect("source repo differs");
    assert_eq!(source.owner, "example-org");
    assert_eq!(source.repo, "wordpress-playground");
}

#[test]
fn resolve_repo_allows_triage_remote_without_git_source_remote() {
    let component_ref = ComponentRef::new(
        "playground".to_string(),
        "/tmp/not-a-git-repo".to_string(),
        None,
        Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
        "rig:studio".to_string(),
    );

    let resolved = resolve_repo(&component_ref).unwrap();

    assert_eq!(resolved.repo.owner, "WordPress");
    assert_eq!(resolved.repo.repo, "wordpress-playground");
    assert!(resolved.source_repo.is_none());
}

#[test]
fn resolve_repo_uses_parent_repo_for_fork_without_triage_remote() {
    let component_ref = ComponentRef::new(
        "playground".to_string(),
        "/tmp/playground".to_string(),
        Some("https://github.com/example-org/wordpress-playground.git".to_string()),
        None,
        "component:playground".to_string(),
    );

    let resolved = resolve_repo_with_parent_resolver(&component_ref, |repo| {
        assert_eq!(repo.owner, "example-org");
        assert_eq!(repo.repo, "wordpress-playground");
        Ok(Some(GitHubRepo {
            host: "github.com".to_string(),
            owner: "WordPress".to_string(),
            repo: "wordpress-playground".to_string(),
        }))
    })
    .unwrap();

    assert_eq!(resolved.repo.owner, "WordPress");
    assert_eq!(resolved.repo.repo, "wordpress-playground");
    assert!(resolved.triage_remote_url.is_none());
    let source = resolved.source_repo.expect("source repo is fork");
    assert_eq!(source.owner, "example-org");
    assert_eq!(source.repo, "wordpress-playground");
}

#[test]
fn parse_github_parent_repo_returns_parent_for_fork() {
    let parent = parse_github_parent_repo(
        r#"{
                "isFork": true,
                "parent": {
                    "name": "wordpress-playground",
                    "owner": { "login": "WordPress" }
                }
            }"#,
    )
    .unwrap()
    .expect("fork parent");

    assert_eq!(parent.owner, "WordPress");
    assert_eq!(parent.repo, "wordpress-playground");
}

#[test]
fn parse_github_parent_repo_ignores_non_forks() {
    let parent = parse_github_parent_repo(
        r#"{
                "isFork": false,
                "parent": null
            }"#,
    )
    .unwrap();

    assert!(parent.is_none());
}

#[test]
fn fetch_component_report_surfaces_source_repo_when_triage_differs() {
    let component_ref = ComponentRef::new(
        "playground".to_string(),
        "/tmp/playground".to_string(),
        Some("https://github.com/example-org/wordpress-playground.git".to_string()),
        Some("https://github.com/WordPress/wordpress-playground.git".to_string()),
        "rig:studio".to_string(),
    );
    let resolved = resolve_repo(&component_ref).unwrap();

    let report = fetch_component_report(
        &component_ref,
        resolved,
        &TriageOptions {
            include_issues: false,
            include_prs: false,
            ..Default::default()
        },
        None,
    );

    assert_eq!(report.repo.owner, "WordPress");
    assert_eq!(report.repo.name, "wordpress-playground");
    assert_eq!(
        report.repo.triage_remote_url.as_deref(),
        Some("https://github.com/WordPress/wordpress-playground.git")
    );
    assert_eq!(
        report.repo.source_repo,
        Some(TriageRepoRef {
            owner: "example-org".to_string(),
            name: "wordpress-playground".to_string(),
            url: "https://github.com/example-org/wordpress-playground".to_string(),
        })
    );
}

#[test]
fn component_target_threads_registered_triage_remote_override() {
    crate::test_support::with_isolated_home(|home| {
        let checkout = home.path().join("playground");
        std::fs::create_dir_all(&checkout).unwrap();
        let component_dir = home.path().join(".config/homeboy/components");
        std::fs::create_dir_all(&component_dir).unwrap();
        std::fs::write(
            component_dir.join("playground.json"),
            format!(
                r#"{{
                    "local_path": "{}",
                    "remote_url": "https://github.com/example-org/wordpress-playground.git",
                    "triage_remote_url": "https://github.com/WordPress/wordpress-playground.git"
                }}"#,
                checkout.display()
            ),
        )
        .unwrap();

        let refs =
            resolve_target_components(&TriageTarget::Component("playground".into())).unwrap();

        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].triage_remote_url.as_deref(),
            Some("https://github.com/WordPress/wordpress-playground.git")
        );
        assert_eq!(
            resolve_repo(&refs[0]).unwrap().repo.owner,
            "WordPress".to_string()
        );
    });
}

#[test]
fn rig_target_threads_rig_component_triage_remote_override() {
    crate::test_support::with_isolated_home(|home| {
        let rig_dir = home.path().join(".config/homeboy/rigs");
        std::fs::create_dir_all(&rig_dir).unwrap();
        std::fs::write(
                rig_dir.join("studio.json"),
                r#"{
                    "id": "studio",
                    "components": {
                        "playground": {
                            "path": "/tmp/playground",
                            "remote_url": "https://github.com/example-org/wordpress-playground.git",
                            "triage_remote_url": "https://github.com/WordPress/wordpress-playground.git"
                        }
                    }
                }"#,
            )
            .unwrap();

        let refs = resolve_target_components(&TriageTarget::Rig("studio".into())).unwrap();

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].component_id, "playground");
        assert_eq!(
            refs[0].triage_remote_url.as_deref(),
            Some("https://github.com/WordPress/wordpress-playground.git")
        );
        assert_eq!(
            resolve_repo(&refs[0]).unwrap().repo.owner,
            "WordPress".to_string()
        );
    });
}

#[test]
fn path_target_synthesizes_component_from_git_origin() {
    crate::test_support::with_isolated_home(|home| {
        let checkout = home.path().join("ad-hoc-checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/Extra-Chill/homeboy.git",
            ])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());

        let target = TriageTarget::Path {
            path: checkout.to_string_lossy().into_owned(),
            component_id: None,
        };
        let refs = resolve_target_components(&target).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].component_id, "ad-hoc-checkout");
        assert_eq!(
            refs[0].remote_url.as_deref(),
            Some("https://github.com/Extra-Chill/homeboy.git")
        );
        let repo = resolve_repo(&refs[0]).unwrap().repo;
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    });
}

#[test]
fn path_target_uses_explicit_component_id_when_provided() {
    crate::test_support::with_isolated_home(|home| {
        let checkout = home.path().join("checkout-dir");
        std::fs::create_dir_all(&checkout).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:Extra-Chill/homeboy.git",
            ])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());

        let target = TriageTarget::Path {
            path: checkout.to_string_lossy().into_owned(),
            component_id: Some("homeboy".into()),
        };
        let refs = resolve_target_components(&target).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].component_id, "homeboy");
        let repo = resolve_repo(&refs[0]).unwrap().repo;
        assert_eq!(repo.owner, "Extra-Chill");
        assert_eq!(repo.repo, "homeboy");
    });
}

#[test]
fn path_target_surfaces_remote_url_is_not_github_for_non_github_origin() {
    crate::test_support::with_isolated_home(|home| {
        let checkout = home.path().join("non-github");
        std::fs::create_dir_all(&checkout).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["remote", "add", "origin", "https://gitlab.com/foo/bar.git"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());

        let target = TriageTarget::Path {
            path: checkout.to_string_lossy().into_owned(),
            component_id: None,
        };
        let refs = resolve_target_components(&target).unwrap();
        let err = resolve_repo(&refs[0]).unwrap_err();
        assert_eq!(err, "remote_url_is_not_github");
    });
}

#[test]
fn path_target_rejects_missing_directory() {
    let target = TriageTarget::Path {
        path: "/definitely/does/not/exist/triage-path-test".into(),
        component_id: None,
    };
    let err = resolve_target_components(&target).unwrap_err();
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
}

#[test]
fn path_target_rejects_non_git_directory() {
    crate::test_support::with_isolated_home(|home| {
        let checkout = home.path().join("not-a-git-repo");
        std::fs::create_dir_all(&checkout).unwrap();

        let target = TriageTarget::Path {
            path: checkout.to_string_lossy().into_owned(),
            component_id: None,
        };
        let err = resolve_target_components(&target).unwrap_err();
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    });
}
