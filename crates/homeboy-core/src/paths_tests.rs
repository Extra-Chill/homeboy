//! Integration tests for path resolution that exercise the `homeboy-paths`
//! crate through `crate::paths`, including the config-override behavior
//! wired via the resolver hook (see the paths<->defaults cycle break). These
//! live in the monolith rather than the leaf crate because they depend on
//! `test_support` and the `defaults` config layer.
#![cfg(test)]

use crate::paths::{
    artifact_root, authorize_remote_artifact_path, expand_tilde_path, homeboy_data,
    join_remote_child, join_remote_path, local_path_is_contained, normalize_local_path,
    resolve_contained_local_path, resolve_optional_base_path, resolve_path_string,
    runner_session_file, runner_sessions_dir, set_artifact_root_override,
    set_config_artifact_root_resolver, RemotePathAuthorizationError, RemotePathRootContainment,
    HOMEBOY_DATA_DIR_ENV,
};
use crate::test_support::with_isolated_home;
use std::path::{Path, PathBuf};

#[test]
fn join_remote_path_allows_absolute_paths_without_base() {
    assert_eq!(
        join_remote_path(None, "/var/log/syslog").unwrap(),
        "/var/log/syslog"
    );
}

#[test]
fn artifact_root_defaults_under_homeboy_data() {
    with_isolated_home(|home| {
        assert_eq!(
            artifact_root().expect("artifact root"),
            home.path().join(".local/share/homeboy/artifacts")
        );
    });
}

#[test]
fn homeboy_data_honors_explicit_durable_directory() {
    with_isolated_home(|home| {
        let durable = home.path().join("durable-homeboy-data");
        std::env::set_var(HOMEBOY_DATA_DIR_ENV, &durable);

        assert_eq!(homeboy_data().expect("homeboy data"), durable);

        std::env::remove_var(HOMEBOY_DATA_DIR_ENV);
    });
}

#[test]
fn expand_tilde_path_uses_home_and_preserves_other_paths() {
    with_isolated_home(|home| {
        assert_eq!(expand_tilde_path("~/source"), home.path().join("source"));
        assert_eq!(
            expand_tilde_path("relative/source"),
            PathBuf::from("relative/source")
        );
    });
}

#[test]
fn artifact_root_uses_configured_value() {
    with_isolated_home(|home| {
        // Mirror production startup wiring: register the config resolver so
        // artifact_root() can read the config-level override.
        set_config_artifact_root_resolver(|| crate::defaults::load_config().artifact_root);
        let configured = home.path().join("custom-artifacts");
        crate::defaults::save_config(&crate::defaults::HomeboyConfig {
            artifact_root: Some(configured.to_string_lossy().to_string()),
            ..crate::defaults::HomeboyConfig::default()
        })
        .expect("save config");

        assert_eq!(artifact_root().expect("artifact root"), configured);
    });
}

#[test]
fn artifact_root_prefers_env_over_config() {
    with_isolated_home(|home| {
        set_config_artifact_root_resolver(|| crate::defaults::load_config().artifact_root);
        let configured = home.path().join("config-artifacts");
        let env_root = home.path().join("env-artifacts");
        crate::defaults::save_config(&crate::defaults::HomeboyConfig {
            artifact_root: Some(configured.to_string_lossy().to_string()),
            ..crate::defaults::HomeboyConfig::default()
        })
        .expect("save config");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", &env_root);

        assert_eq!(artifact_root().expect("artifact root"), env_root);
    });
}

#[test]
fn test_set_artifact_root_override() {
    with_isolated_home(|home| {
        let env_root = home.path().join("env-artifacts");
        let override_root = home.path().join("override-artifacts");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", &env_root);
        set_artifact_root_override(Some(override_root.clone()));

        assert_eq!(artifact_root().expect("artifact root"), override_root);
    });
}

#[test]
fn join_remote_path_rejects_relative_paths_without_base() {
    assert!(join_remote_path(None, "file.json").is_err());
}

#[test]
fn join_remote_path_joins_relative_paths() {
    assert_eq!(
        join_remote_path(Some("/var/www/site"), "file.json").unwrap(),
        "/var/www/site/file.json"
    );

    assert_eq!(
        join_remote_path(Some("/var/www/site/"), "file.json").unwrap(),
        "/var/www/site/file.json"
    );
}

#[test]
fn join_remote_child_appends_child() {
    assert_eq!(
        join_remote_child(Some("/var/www/site"), "logs", "error.log").unwrap(),
        "/var/www/site/logs/error.log"
    );

    assert_eq!(
        join_remote_child(Some("/var/www/site"), "/var/log", "syslog").unwrap(),
        "/var/log/syslog"
    );
}

#[test]
fn authorize_remote_artifact_path_checks_lexical_policy() {
    let roots = vec!["/runner/workspace/".to_string()];
    assert_eq!(
        authorize_remote_artifact_path(
            Path::new("relative"),
            &roots,
            RemotePathRootContainment::RemoteString,
        ),
        Err(RemotePathAuthorizationError::NotAbsolute)
    );
    assert_eq!(
        authorize_remote_artifact_path(
            Path::new("/runner/workspace/../secret"),
            &roots,
            RemotePathRootContainment::RemoteString,
        ),
        Err(RemotePathAuthorizationError::ContainsParentDir)
    );
    assert_eq!(
        authorize_remote_artifact_path(
            Path::new("/other/artifact"),
            &roots,
            RemotePathRootContainment::RemoteString,
        ),
        Err(RemotePathAuthorizationError::OutsideAllowedRoots)
    );
    assert!(authorize_remote_artifact_path(
        Path::new("/runner/workspace/out"),
        &roots,
        RemotePathRootContainment::RemoteString,
    )
    .is_ok());
}

#[test]
fn resolve_optional_base_path_trims_and_rejects_empty() {
    assert_eq!(
        resolve_optional_base_path(Some(" /var/www ")),
        Some("/var/www")
    );
    assert_eq!(resolve_optional_base_path(Some("   ")), None);
    assert_eq!(resolve_optional_base_path(None), None);
}

#[test]
fn normalize_local_path_collapses_dot_and_parent_segments() {
    assert_eq!(
        normalize_local_path("/repo/./packages/../src"),
        PathBuf::from("/repo/src")
    );
    assert_eq!(
        normalize_local_path("packages/../src/./lib"),
        PathBuf::from("src/lib")
    );
    assert_eq!(
        normalize_local_path("../../src"),
        PathBuf::from("../../src")
    );
    assert_eq!(normalize_local_path("/../../src"), PathBuf::from("/src"));
}

#[test]
fn local_path_containment_is_component_aware() {
    assert!(local_path_is_contained("/repo", "/repo/dir/file.txt"));
    assert!(local_path_is_contained(
        "/repo",
        "/repo/dir/../manifest.txt"
    ));
    assert!(!local_path_is_contained("/repo", "/repo-other/file"));
    assert!(!local_path_is_contained("/repo", "/repo/../etc/passwd"));
}

#[test]
fn resolve_contained_local_path_resolves_relative_paths_under_root() {
    assert_eq!(
        resolve_contained_local_path("/repo", "dir/../manifest.txt", "cwd").unwrap(),
        PathBuf::from("/repo/manifest.txt")
    );
}

#[test]
fn resolve_contained_local_path_rejects_parent_escape() {
    let err = resolve_contained_local_path("/repo/worktree", "../secrets", "cwd")
        .expect_err("parent escape should fail");

    assert!(err.to_string().contains("escapes root '/repo/worktree'"));
}

#[test]
fn resolve_path_handles_relative() {
    let result = resolve_path_string("/base", "relative/path");
    assert_eq!(result, "/base/relative/path");
}

#[test]
fn test_runner_sessions_dir_under_homeboy_dir() {
    let path = runner_sessions_dir().expect("runner_sessions_dir resolves");
    assert!(path.ends_with("runner-sessions"), "got {}", path.display());
    assert!(path.parent().expect("parent").ends_with("homeboy"));
}

#[test]
fn test_runner_session_file_uses_id_filename() {
    let path = runner_session_file("lab-box").expect("runner_session_file resolves");
    assert_eq!(
        path.file_name().and_then(|s| s.to_str()),
        Some("lab-box.json")
    );
    assert_eq!(
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str()),
        Some("runner-sessions")
    );
}
