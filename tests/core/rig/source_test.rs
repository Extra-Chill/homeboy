//! Rig source lifecycle tests. Covers `src/core/rig/source.rs`.

use crate::rig::{install, list_sources, remove_source, update_all_sources, update_source_for_rig};
use crate::test_support::HomeGuard;
use std::fs;
use std::path::Path;
use std::process::Command;

fn write_rig(package: &Path, id: &str, body: &str) -> std::path::PathBuf {
    let rig_dir = package.join("rigs").join(id);
    fs::create_dir_all(&rig_dir).expect("rig dir");
    let rig_path = rig_dir.join("rig.json");
    fs::write(&rig_path, body).expect("rig json");
    rig_path
}

fn minimal_rig(id: &str) -> String {
    format!(
        r#"{{
            "id": "{}",
            "description": "{} rig",
            "components": {{
                "app": {{ "path": "${{env.DEV_ROOT}}/{}" }}
            }},
            "pipeline": {{
                "check": [{{ "kind": "check", "label": "app exists", "file": "${{components.app.path}}" }}]
            }}
        }}"#,
        id, id, id
    )
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_package(package: &Path, message: &str) {
    run_git(package, &["add", "."]);
    run_git(
        package,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

fn create_bare_source(package: &Path) -> tempfile::TempDir {
    run_git(package, &["init"]);
    commit_package(package, "initial rigs");

    let bare = tempfile::tempdir().expect("bare parent");
    let source_path = bare.path().join("rig-package.git");
    let output = Command::new("git")
        .args([
            "clone",
            "--bare",
            package.to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("git clone --bare");
    assert!(output.status.success());
    bare
}

#[test]
fn test_list_sources() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));

    install(package.path().to_str().unwrap(), None, true).expect("install all");

    let result = list_sources().expect("sources");
    assert_eq!(result.invalid.len(), 0);
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].source, package.path().to_string_lossy());
    assert!(result.sources[0].linked);
    assert_eq!(result.sources[0].rigs.len(), 2);
    assert_eq!(result.sources[0].rigs[0].id, "alpha");
    assert!(result.sources[0].rigs[0].config_present);
    assert!(result.sources[0].rigs[0].config_owned);
    assert_eq!(result.sources[0].rigs[1].id, "beta");
}

#[test]
fn test_remove_source() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));

    install(package.path().to_str().unwrap(), None, true).expect("install all");
    let manual = crate::paths::rig_config("manual").expect("manual rig path");
    fs::write(&manual, minimal_rig("manual")).expect("manual rig");

    let result = remove_source(&package.path().to_string_lossy()).expect("remove source");
    assert_eq!(result.removed.len(), 2);
    assert!(result.skipped.is_empty());
    assert!(result.removed_package_path.is_none());
    assert!(!crate::paths::rig_config("alpha").unwrap().exists());
    assert!(!crate::paths::rig_config("beta").unwrap().exists());
    assert!(!crate::paths::rig_source_metadata("alpha").unwrap().exists());
    assert!(!crate::paths::rig_source_metadata("beta").unwrap().exists());
    assert!(manual.exists());
    assert!(package.path().exists());
}

#[test]
fn remove_source_preserves_replaced_config_but_drops_metadata() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    install(package.path().to_str().unwrap(), None, false).expect("install");
    let config = crate::paths::rig_config("alpha").expect("rig config");
    fs::remove_file(&config).expect("remove symlink");
    fs::write(&config, minimal_rig("replacement")).expect("replacement rig");

    let result = remove_source(&package.path().to_string_lossy()).expect("remove source");
    assert!(result.removed.is_empty());
    assert_eq!(result.skipped.len(), 1);
    assert!(config.exists());
    assert!(!crate::paths::rig_source_metadata("alpha").unwrap().exists());
}

#[test]
fn remove_source_treats_copied_config_as_owned_when_contents_match() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let source = write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    install(package.path().to_str().unwrap(), None, false).expect("install");
    let config = crate::paths::rig_config("alpha").expect("rig config");
    fs::remove_file(&config).expect("remove symlink");
    fs::copy(&source, &config).expect("copy rig config");

    let listed = list_sources().expect("sources");
    assert!(listed.sources[0].rigs[0].config_owned);

    let result = remove_source(&package.path().to_string_lossy()).expect("remove source");
    assert_eq!(result.removed.len(), 1);
    assert!(result.skipped.is_empty());
    assert!(!config.exists());
    assert!(!crate::paths::rig_source_metadata("alpha").unwrap().exists());
}

#[test]
fn sources_list_reports_corrupt_metadata_and_missing_configs() {
    let _home = HomeGuard::new();
    fs::create_dir_all(crate::paths::rig_sources().expect("sources dir")).expect("sources dir");
    fs::write(
        crate::paths::rig_source_metadata("broken").expect("broken metadata"),
        "not json",
    )
    .expect("broken metadata");
    fs::write(
        crate::paths::rig_source_metadata("missing").expect("missing metadata"),
        r#"{
            "source": "/tmp/package",
            "package_path": "/tmp/package",
            "rig_path": "/tmp/package/rigs/missing/rig.json",
            "linked": true
        }"#,
    )
    .expect("missing metadata");

    let result = list_sources().expect("sources");
    assert_eq!(result.invalid.len(), 1);
    assert_eq!(result.invalid[0].id, "broken");
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].rigs[0].id, "missing");
    assert!(!result.sources[0].rigs[0].config_present);
    assert!(!result.sources[0].rigs[0].config_owned);
}

#[test]
fn update_git_source_fast_forwards_package_and_refreshes_metadata() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let source_rig = write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    let bare = create_bare_source(package.path());
    let source = bare
        .path()
        .join("rig-package.git")
        .to_string_lossy()
        .to_string();

    install(&source, None, false).expect("install");
    let before = crate::rig::read_source_metadata("alpha")
        .expect("metadata")
        .source_revision;

    fs::write(
        &source_rig,
        minimal_rig("alpha").replace("alpha rig", "alpha rig updated"),
    )
    .expect("update rig");
    commit_package(package.path(), "update alpha");
    run_git(package.path(), &["push", &source, "HEAD:main"]);

    let result = update_source_for_rig("alpha").expect("update rig source");

    assert_eq!(result.updated.len(), 1);
    assert!(result.skipped.is_empty());
    assert_eq!(result.updated[0].id, "alpha");
    assert_eq!(result.updated[0].previous_revision, before);
    assert_ne!(result.updated[0].source_revision, before);
    let installed =
        fs::read_to_string(crate::paths::rig_config("alpha").unwrap()).expect("installed rig");
    assert!(installed.contains("alpha rig updated"));
    assert_eq!(
        crate::rig::read_source_metadata("alpha")
            .expect("metadata")
            .source_revision,
        result.updated[0].source_revision
    );
}

#[test]
fn update_all_skips_linked_local_sources() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    install(package.path().to_str().unwrap(), None, false).expect("install linked");
    let result = update_all_sources().expect("update all");

    assert!(result.updated.is_empty());
    assert_eq!(result.skipped.len(), 1);
    assert_eq!(result.skipped[0].id, "alpha");
    assert!(result.skipped[0].reason.contains("linked local sources"));
}

#[test]
fn update_single_linked_local_source_errors() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    install(package.path().to_str().unwrap(), None, false).expect("install linked");
    let err = update_source_for_rig("alpha").expect_err("linked update error");

    assert!(err.message.contains("linked local source"));
}
