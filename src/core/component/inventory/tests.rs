#![cfg(test)]

use super::*;
use crate::core::component::Component;
use std::fs;
use std::process::Command;
use std::sync::MutexGuard;
use tempfile::TempDir;

// Tests that override `HOME` to redirect `paths::components()` are
// inherently racy when run in parallel because environment variables
// are process-wide. Rather than `#[ignore]`-ing them (which skips
// coverage in default `cargo test` runs), we serialize every test in
// every module that touches `HOME` through `test_support::home_env_guard()`.
// Acquire the guard via `with_home_override()` before any `set_var("HOME", ...)`
// and the guard's `Drop` restores the previous value; parallel test runners
// block on the mutex instead of racing on the env var.
//
/// Serialized guard for tests that override `HOME`.
///
/// Acquires the shared HOME env guard, snapshots the current `HOME`, and
/// installs the test-supplied override. When the guard is dropped the
/// previous `HOME` is restored and the lock is released.
///
/// Panics on a poisoned mutex, which can only happen if a previous
/// test panicked while holding the guard — in that case the test
/// runner is already reporting a failure, so a follow-up panic here
/// is fine.
struct HomeGuard {
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

fn with_home_override(new_home: &std::path::Path) -> HomeGuard {
    let lock = crate::test_support::home_env_guard();
    let previous = std::env::var("HOME").ok();
    unsafe { std::env::set_var("HOME", new_home.to_string_lossy().as_ref()) };
    HomeGuard {
        previous,
        _lock: lock,
    }
}

/// Helper: create a standalone component JSON file in a directory.
fn write_standalone_json(dir: &std::path::Path, id: &str, local_path: &str) {
    let path = dir.join(format!("{}.json", id));
    let json = serde_json::json!({
        "local_path": local_path,
        "remote_path": format!("wp-content/plugins/{}", id),
        "extensions": { "wordpress": {} },
        "auto_cleanup": false
    });
    fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
}

fn init_git_repo(path: &std::path::Path) {
    fs::create_dir_all(path).expect("repo dir");
    let output = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(path)
        .output()
        .expect("git init should run");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_portable_id(path: &std::path::Path, id: &str) {
    fs::write(
        path.join("homeboy.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "id": id })).unwrap(),
    )
    .expect("homeboy.json");
}

#[test]
fn write_standalone_registration_rejects_blank_id() {
    let component = Component::new(
        String::new(),
        "/tmp/test".to_string(),
        "wp-content/plugins/test".to_string(),
        None,
    );

    let result = write_standalone_registration(&component);
    assert!(result.is_err(), "Should reject blank ID");
}

#[test]
fn standalone_prefers_portable_config_when_available() {
    // This test calls load_standalone_components() which reads from
    // paths::components(). We set HOME to an isolated temp dir.
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    // Also create empty projects dir so inventory doesn't fail
    let projects_dir = dir.path().join(".config").join("homeboy").join("projects");
    fs::create_dir_all(&projects_dir).unwrap();

    // Create a repo directory with homeboy.json
    let repo_dir = dir.path().join("my-plugin");
    fs::create_dir_all(&repo_dir).unwrap();

    let portable = serde_json::json!({
        "id": "my-plugin",
        "version_targets": [{"file": "plugin.php", "pattern": "Version:\\s*([0-9.]+)"}],
        "changelog_target": "CHANGELOG.md",
        "extensions": {"wordpress": {}}
    });
    fs::write(
        repo_dir.join("homeboy.json"),
        serde_json::to_string_pretty(&portable).unwrap(),
    )
    .unwrap();

    // Create standalone registration pointing to repo
    let standalone = serde_json::json!({
        "local_path": repo_dir.to_string_lossy(),
        "remote_path": "wp-content/plugins/my-plugin"
    });
    fs::write(
        config_components.join("my-plugin.json"),
        serde_json::to_string_pretty(&standalone).unwrap(),
    )
    .unwrap();

    // Override HOME via the serialized guard so parallel tests can't
    // race on this process-global env var. See the HOME_LOCK comment.
    let _home = with_home_override(dir.path());

    let result = load_standalone_components();

    let components = result.unwrap();
    let plugin = components
        .iter()
        .find(|c| c.id == "my-plugin")
        .expect("Should find my-plugin");

    // Should have data from portable config
    assert!(
        plugin.version_targets.is_some(),
        "Should have version_targets from portable config"
    );
    assert_eq!(
        plugin.changelog_target.as_deref(),
        Some("CHANGELOG.md"),
        "Should have changelog_target from portable config"
    );

    // Should have remote_path from standalone (not in portable)
    assert_eq!(
        plugin.remote_path, "wp-content/plugins/my-plugin",
        "Should inherit remote_path from standalone registration"
    );
}

#[test]
fn portable_config_fields_override_standalone_registration() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    let repo_dir = dir.path().join("repo-owned-plugin");
    fs::create_dir_all(&repo_dir).unwrap();

    fs::write(
        repo_dir.join("homeboy.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "repo-owned-plugin",
            "remote_path": "portable/remote-path",
            "build_artifact": "portable.zip",
            "extensions": { "portable-extension": {} }
        }))
        .unwrap(),
    )
    .unwrap();

    fs::write(
        config_components.join("repo-owned-plugin.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "local_path": repo_dir.to_string_lossy(),
            "remote_path": "standalone/remote-path",
            "build_artifact": "standalone.zip",
            "extensions": { "standalone-extension": {} }
        }))
        .unwrap(),
    )
    .unwrap();

    let _home = with_home_override(dir.path());

    let components = load_standalone_components().unwrap();
    let plugin = components
        .iter()
        .find(|c| c.id == "repo-owned-plugin")
        .expect("component should load");

    assert_eq!(plugin.local_path, repo_dir.to_string_lossy());
    assert_eq!(plugin.remote_path, "portable/remote-path");
    assert_eq!(plugin.build_artifact.as_deref(), Some("portable.zip"));
    assert!(plugin
        .extensions
        .as_ref()
        .is_some_and(|extensions| extensions.contains_key("portable-extension")));
    assert!(!plugin
        .extensions
        .as_ref()
        .is_some_and(|extensions| extensions.contains_key("standalone-extension")));
}

#[test]
fn load_standalone_skips_missing_local_path() {
    let dir = TempDir::new().unwrap();

    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    // Write a component with empty local_path
    let json = serde_json::json!({
        "local_path": "",
        "remote_path": "wp-content/plugins/broken"
    });
    fs::write(
        config_components.join("broken.json"),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .unwrap();

    let _home = with_home_override(dir.path());
    let result = load_standalone_components();

    let components = result.unwrap();
    assert!(
        components.is_empty(),
        "Should skip components with empty local_path"
    );
}

#[test]
fn load_standalone_skips_non_json_files() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    // Create a non-JSON file
    fs::write(config_components.join("readme.txt"), "not a component").unwrap();
    // Create an invalid JSON file
    fs::write(config_components.join("broken.json"), "not valid json").unwrap();

    let _home = with_home_override(dir.path());
    let result = load_standalone_components();

    let components = result.unwrap();
    assert!(
        components.is_empty(),
        "Should skip non-JSON and invalid JSON files"
    );
}

#[test]
fn load_standalone_reads_json_files() {
    let dir = TempDir::new().unwrap();

    // Create the ~/.config/homeboy/components/ directory structure
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    // Create a fake component directory
    let repo_dir = dir.path().join("my-plugin");
    fs::create_dir_all(&repo_dir).unwrap();

    write_standalone_json(&config_components, "my-plugin", &repo_dir.to_string_lossy());

    let _home = with_home_override(dir.path());
    let result = load_standalone_components();

    let components = result.unwrap();
    assert!(
        components.iter().any(|c| c.id == "my-plugin"),
        "Should find my-plugin from standalone files. Found: {:?}",
        components.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

#[test]
fn stale_standalone_path_discovers_renamed_sibling_portable_component() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    let workspace = dir.path().join("workspace");
    let stale_path = workspace.join("old-plugin");
    let renamed_path = workspace.join("new-plugin");
    fs::create_dir_all(&renamed_path).unwrap();

    let standalone = serde_json::json!({
        "local_path": stale_path.to_string_lossy(),
        "remote_path": "wp-content/plugins/old-plugin"
    });
    fs::write(
        config_components.join("old-plugin.json"),
        serde_json::to_string_pretty(&standalone).unwrap(),
    )
    .unwrap();

    let portable = serde_json::json!({
        "id": "new-plugin",
        "local_path": renamed_path.to_string_lossy(),
        "remote_path": "wp-content/plugins/new-plugin",
        "changelog_target": "CHANGELOG.md"
    });
    fs::write(
        renamed_path.join("homeboy.json"),
        serde_json::to_string_pretty(&portable).unwrap(),
    )
    .unwrap();

    let _home = with_home_override(dir.path());
    let components = load_standalone_components().unwrap();

    let renamed = components
        .iter()
        .find(|component| component.id == "new-plugin")
        .expect("renamed sibling component should be discovered from homeboy.json");
    assert_eq!(renamed.local_path, renamed_path.to_string_lossy());
    assert!(
        !components
            .iter()
            .any(|component| component.id == "old-plugin"),
        "stale standalone path should not re-register the old component id"
    );
}

#[test]
fn reconcile_reports_safe_sibling_repair_without_applying() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    let workspace = dir.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let stale_path = workspace.join("old-homeboy");
    let checkout = workspace.join("homeboy");
    fs::create_dir_all(checkout.join(".git")).unwrap();
    fs::write(
        checkout.join("homeboy.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "id": "homeboy" })).unwrap(),
    )
    .unwrap();
    write_standalone_json(&config_components, "homeboy", &stale_path.to_string_lossy());

    let _home = with_home_override(dir.path());
    let report = reconcile_standalone_registration("homeboy", false).unwrap();

    assert_eq!(report.status, "missing");
    assert_eq!(
        report.discovered_local_path.as_deref(),
        Some(checkout.to_string_lossy().as_ref())
    );
    assert!(!report.applied);

    let raw = fs::read_to_string(config_components.join("homeboy.json")).unwrap();
    assert!(raw.contains(stale_path.to_string_lossy().as_ref()));
}

#[test]
fn reconcile_apply_updates_stale_local_path_when_candidate_is_unique() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    let workspace = dir.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let stale_path = workspace.join("old-homeboy");
    let checkout = workspace.join("homeboy");
    fs::create_dir_all(checkout.join(".git")).unwrap();
    fs::write(
        checkout.join("homeboy.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "id": "homeboy" })).unwrap(),
    )
    .unwrap();
    write_standalone_json(&config_components, "homeboy", &stale_path.to_string_lossy());

    let _home = with_home_override(dir.path());
    let report = reconcile_standalone_registration("homeboy", true).unwrap();

    assert!(report.applied);
    let raw = fs::read_to_string(config_components.join("homeboy.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        json.get("local_path").and_then(|value| value.as_str()),
        Some(checkout.to_string_lossy().as_ref())
    );
}

#[test]
fn reconcile_flags_relative_local_path_and_repairs_to_absolute() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    // A stable checkout lives under the workspace root ($HOME/Developer) so the
    // existing candidate discovery can resolve the absolute path.
    let checkout = dir.path().join("Developer").join("homeboy");
    fs::create_dir_all(checkout.join(".git")).unwrap();
    fs::write(
        checkout.join("homeboy.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "id": "homeboy" })).unwrap(),
    )
    .unwrap();

    // Registration stored with a RELATIVE local_path — the exact shape that
    // fails `release` with "cannot be resolved". (#6938)
    write_standalone_json(&config_components, "homeboy", "homeboy");

    let _home = with_home_override(dir.path());
    let report = reconcile_standalone_registration("homeboy", false).unwrap();

    assert_eq!(report.status, "relative_local_path");
    assert_eq!(
        report.discovered_local_path.as_deref(),
        Some(checkout.to_string_lossy().as_ref())
    );
    assert!(!report.applied);

    let applied = reconcile_standalone_registration("homeboy", true).unwrap();
    assert!(applied.applied);
    let raw = fs::read_to_string(config_components.join("homeboy.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let persisted = json
        .get("local_path")
        .and_then(|value| value.as_str())
        .unwrap();
    assert!(
        std::path::Path::new(persisted).is_absolute(),
        "repaired local_path must be absolute, got {persisted}"
    );
    assert_eq!(persisted, checkout.to_string_lossy().as_ref());
}

#[test]
fn local_path_diagnostic_flags_temp_checkout_and_reports_stable_candidate() {
    let dir = TempDir::new().unwrap();
    let stable_checkout = dir.path().join("Developer").join("homeboy");
    let temp_checkout = dir.path().join("opencode").join("homeboy-issue-4202-temp");
    init_git_repo(&stable_checkout);
    init_git_repo(&temp_checkout);
    write_portable_id(&stable_checkout, "homeboy");
    write_portable_id(&temp_checkout, "homeboy");

    let _home = with_home_override(dir.path());
    let component = Component::new(
        "homeboy".to_string(),
        temp_checkout.to_string_lossy().to_string(),
        String::new(),
        None,
    );

    let diagnostic = local_path_diagnostic(&component);

    assert_eq!(diagnostic.status, "temp_checkout");
    assert!(diagnostic.exists);
    assert!(diagnostic.is_git_checkout);
    assert!(diagnostic.is_temp_checkout);
    assert_eq!(
        diagnostic.git_root.as_deref(),
        Some(temp_checkout.to_string_lossy().as_ref())
    );
    assert_eq!(
        diagnostic.discovered_candidates,
        vec![stable_checkout.to_string_lossy().to_string()]
    );
    assert!(diagnostic
        .warning
        .as_deref()
        .unwrap_or_default()
        .contains("temporary/opencode checkout"));
    assert!(diagnostic
        .repair_command
        .as_deref()
        .unwrap_or_default()
        .contains("homeboy component set homeboy --local-path"));
}

#[test]
fn reconcile_repairs_temp_checkout_to_unique_stable_candidate() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();
    let stable_checkout = dir.path().join("Developer").join("homeboy");
    let temp_checkout = dir.path().join("opencode").join("homeboy-temp");
    init_git_repo(&stable_checkout);
    init_git_repo(&temp_checkout);
    write_portable_id(&stable_checkout, "homeboy");
    write_portable_id(&temp_checkout, "homeboy");
    write_standalone_json(
        &config_components,
        "homeboy",
        &temp_checkout.to_string_lossy(),
    );

    let _home = with_home_override(dir.path());
    let report = reconcile_standalone_registration("homeboy", true).unwrap();

    assert_eq!(report.status, "temp_checkout");
    assert_eq!(
        report.discovered_local_path.as_deref(),
        Some(stable_checkout.to_string_lossy().as_ref())
    );
    assert!(report.applied);
    let raw = fs::read_to_string(config_components.join("homeboy.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        json.get("local_path").and_then(|value| value.as_str()),
        Some(stable_checkout.to_string_lossy().as_ref())
    );
}

#[test]
fn write_standalone_creates_and_reads_back() {
    let dir = TempDir::new().unwrap();
    let config_dir = dir.path().join(".config").join("homeboy");
    fs::create_dir_all(&config_dir).unwrap();

    let _home = with_home_override(dir.path());

    let repo_dir = dir.path().join("test-plugin");
    fs::create_dir_all(&repo_dir).unwrap();

    let component = Component::new(
        "test-plugin".to_string(),
        repo_dir.to_string_lossy().to_string(),
        "wp-content/plugins/test-plugin".to_string(),
        None,
    );

    let write_result = write_standalone_registration(&component);
    assert!(
        write_result.is_ok(),
        "Should write successfully: {:?}",
        write_result.err()
    );

    // Verify we can read it back
    let read_result = load_standalone_components();

    assert!(read_result.is_ok());
    let components = read_result.unwrap();
    assert!(
        components.iter().any(|c| c.id == "test-plugin"),
        "Should find test-plugin after writing. Found: {:?}",
        components.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

#[test]
fn write_standalone_preserves_existing_fields() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    // Write an existing registration with extra fields
    let existing = serde_json::json!({
        "local_path": "/old/path",
        "remote_path": "wp-content/plugins/my-comp",
        "extra_field": "preserve-me"
    });
    fs::write(
        config_components.join("my-comp.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    let _home = with_home_override(dir.path());

    let component = Component::new(
        "my-comp".to_string(),
        "/new/path".to_string(),
        "wp-content/plugins/my-comp".to_string(),
        None,
    );

    let result = write_standalone_registration(&component);

    assert!(result.is_ok());

    let content = fs::read_to_string(config_components.join("my-comp.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();

    // local_path should be updated
    assert_eq!(
        json.get("local_path").and_then(|v| v.as_str()),
        Some("/new/path"),
        "local_path should be updated"
    );
    // extra_field should be preserved
    assert_eq!(
        json.get("extra_field").and_then(|v| v.as_str()),
        Some("preserve-me"),
        "unknown fields should be preserved"
    );
}

#[test]
fn rename_standalone_moves_pointer_to_new_component_id() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    let existing = serde_json::json!({
        "local_path": "/old/path",
        "remote_path": "target/release/old-id",
        "extra_field": "preserve-me"
    });
    fs::write(
        config_components.join("old-id.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    let _home = with_home_override(dir.path());

    let component = Component::new(
        "new-id".to_string(),
        "/new/path".to_string(),
        "target/release/new-id".to_string(),
        None,
    );

    rename_standalone_registration("old-id", &component).unwrap();

    assert!(!config_components.join("old-id.json").exists());
    let content = fs::read_to_string(config_components.join("new-id.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(
        json.get("local_path").and_then(|v| v.as_str()),
        Some("/new/path")
    );
    assert_eq!(
        json.get("remote_path").and_then(|v| v.as_str()),
        Some("target/release/new-id")
    );
    assert_eq!(
        json.get("extra_field").and_then(|v| v.as_str()),
        Some("preserve-me")
    );
}

#[test]
fn write_standalone_rejects_preserved_invalid_remote_url() {
    let dir = TempDir::new().unwrap();
    let config_components = dir
        .path()
        .join(".config")
        .join("homeboy")
        .join("components");
    fs::create_dir_all(&config_components).unwrap();

    let existing = serde_json::json!({
        "local_path": "/old/path",
        "remote_url": "/Users/user/Developer/homeboy"
    });
    fs::write(
        config_components.join("my-comp.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    let _home = with_home_override(dir.path());

    let component = Component::new(
        "my-comp".to_string(),
        "/new/path".to_string(),
        String::new(),
        None,
    );

    let result = write_standalone_registration(&component);

    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().code.as_str(),
        "validation.invalid_argument"
    );
}
