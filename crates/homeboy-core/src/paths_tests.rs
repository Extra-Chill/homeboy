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

#[cfg(unix)]
#[test]
fn canonical_or_normalized_local_path_resolves_symlink_identity() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("temp root");
    let target = root.path().join("target");
    std::fs::create_dir(&target).expect("create target");
    let alias = root.path().join("alias");
    symlink(&target, &alias).expect("symlink target");

    assert_eq!(
        crate::paths::canonical_or_normalized_local_path(&target),
        crate::paths::canonical_or_normalized_local_path(&alias)
    );
}

#[test]
fn canonical_or_normalized_local_path_preserves_nonexistent_path_normalization() {
    assert_eq!(
        crate::paths::canonical_or_normalized_local_path("/missing/root/../workload.bench.mjs"),
        PathBuf::from("/missing/workload.bench.mjs")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn canonical_or_normalized_local_path_resolves_the_var_alias() {
    assert_eq!(
        crate::paths::canonical_or_normalized_local_path("/var"),
        crate::paths::canonical_or_normalized_local_path("/private/var")
    );
}

#[test]
fn artifact_root_defaults_under_homeboy_data() {
    with_isolated_home(|home| {
        // `with_isolated_home` sets `HOMEBOY_DATA_DIR` to `<home>/data`, and
        // `homeboy_data()` checks it *before* the XDG fallback -- so the
        // default this test exists to pin is unreachable until the override is
        // cleared. Clear it explicitly, matching
        // `homeboy_data_honors_explicit_durable_directory` below, which sets
        // and removes the same variable.
        let restore = std::env::var(HOMEBOY_DATA_DIR_ENV).ok();
        std::env::remove_var(HOMEBOY_DATA_DIR_ENV);

        let resolved = artifact_root().expect("artifact root");

        match restore {
            Some(value) => std::env::set_var(HOMEBOY_DATA_DIR_ENV, value),
            None => std::env::remove_var(HOMEBOY_DATA_DIR_ENV),
        }

        assert_eq!(resolved, home.path().join(".local/share/homeboy/artifacts"));
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
fn artifact_root_precedence_is_override_then_env_then_config() {
    with_isolated_home(|home| {
        // Mirror production startup wiring: register the config resolver so
        // artifact_root() can read the config-level override.
        set_config_artifact_root_resolver(|| crate::defaults::load_config().artifact_root);
        let configured = home.path().join("config-artifacts");
        crate::defaults::save_config(&crate::defaults::HomeboyConfig {
            artifact_root: Some(configured.to_string_lossy().to_string()),
            ..crate::defaults::HomeboyConfig::default()
        })
        .expect("save config");
        assert_eq!(artifact_root().expect("artifact root"), configured);

        let env_root = home.path().join("env-artifacts");
        std::env::set_var("HOMEBOY_ARTIFACT_ROOT", &env_root);
        assert_eq!(artifact_root().expect("artifact root"), env_root);

        let override_root = home.path().join("override-artifacts");
        set_artifact_root_override(Some(override_root.clone()));
        assert_eq!(artifact_root().expect("artifact root"), override_root);
    });
}

#[test]
fn remote_path_joining_anchors_relative_paths_and_keeps_absolute_ones() {
    for (base, path, expected) in [
        (None, "/var/log/syslog", "/var/log/syslog"),
        (
            Some("/var/www/site"),
            "file.json",
            "/var/www/site/file.json",
        ),
        (
            Some("/var/www/site/"),
            "file.json",
            "/var/www/site/file.json",
        ),
    ] {
        assert_eq!(join_remote_path(base, path).unwrap(), expected, "{path}");
    }
    assert!(join_remote_path(None, "file.json").is_err());

    assert_eq!(
        join_remote_child(Some("/var/www/site"), "logs", "error.log").unwrap(),
        "/var/www/site/logs/error.log"
    );
    assert_eq!(
        join_remote_child(Some("/var/www/site"), "/var/log", "syslog").unwrap(),
        "/var/log/syslog"
    );
    assert_eq!(
        resolve_path_string("/base", "relative/path"),
        "/base/relative/path"
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
fn resolve_contained_local_path_resolves_under_root_and_rejects_parent_escape() {
    assert_eq!(
        resolve_contained_local_path("/repo", "dir/../manifest.txt", "cwd").unwrap(),
        PathBuf::from("/repo/manifest.txt")
    );

    let err = resolve_contained_local_path("/repo/worktree", "../secrets", "cwd")
        .expect_err("parent escape should fail");
    assert!(err.to_string().contains("escapes root '/repo/worktree'"));
}

#[test]
fn runner_session_file_is_named_by_id_under_runner_sessions() {
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
    assert_eq!(
        path.parent(),
        Some(runner_sessions_dir().unwrap().as_path())
    );
}
