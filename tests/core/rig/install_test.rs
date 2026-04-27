//! Rig install lifecycle tests. Covers `src/core/rig/install.rs`.

use crate::rig::{declared_id, discover_rigs, install, list, list_ids, load, read_source_metadata};
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

fn write_single_rig(dir: &Path, id: &str, body: &str) -> std::path::PathBuf {
    fs::create_dir_all(dir).expect("single rig dir");
    let rig_path = dir.join("rig.json");
    fs::write(&rig_path, body).expect("rig json");
    assert!(body.contains(id));
    rig_path
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

fn commit_package(package: &Path) {
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
            "update rigs",
        ],
    );
}

fn bare_package(package: &Path) -> tempfile::TempDir {
    run_git(package, &["init"]);
    commit_package(package);

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
fn test_discover_rigs_from_convention_paths() {
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));

    let rigs = discover_rigs(package.path()).expect("discover");
    assert_eq!(rigs.len(), 2);
    assert_eq!(rigs[0].id, "alpha");
    assert_eq!(rigs[0].description, "alpha rig");
    assert_eq!(rigs[1].id, "beta");
}

#[test]
fn discover_single_rig_directory_with_rig_json() {
    let package = tempfile::tempdir().expect("package");
    fs::write(package.path().join("rig.json"), minimal_rig("solo")).expect("rig json");

    let rigs = discover_rigs(package.path()).expect("discover");
    assert_eq!(rigs.len(), 1);
    assert_eq!(rigs[0].id, "solo");
    assert_eq!(rigs[0].rig_path, package.path().join("rig.json"));
}

#[test]
fn test_read_source_metadata_after_local_install() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let source = write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    let result = install(package.path().to_str().unwrap(), None, false).expect("install");
    assert!(result.linked);
    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].id, "alpha");

    let installed = crate::paths::rig_config("alpha").expect("rig path");
    assert!(installed.exists());
    #[cfg(unix)]
    assert_eq!(fs::read_link(&installed).expect("symlink"), source);

    let installed_content = fs::read_to_string(&installed).expect("installed content");
    assert!(installed_content.contains("${env.DEV_ROOT}/alpha"));
    assert!(installed_content.contains("${components.app.path}"));

    let metadata = read_source_metadata("alpha").expect("metadata");
    assert!(metadata.linked);
    assert_eq!(metadata.rig_path, source.to_string_lossy());
}

#[test]
fn install_multi_rig_package_requires_id_or_all() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));

    let err = install(package.path().to_str().unwrap(), None, false).expect_err("error");
    assert!(err.message.contains("multiple rigs"));
    assert!(err.message.contains("alpha"));
    assert!(err.message.contains("beta"));
}

#[test]
fn install_multi_rig_package_can_select_id() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));

    let result = install(package.path().to_str().unwrap(), Some("beta"), false).expect("install");
    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].id, "beta");
    assert!(crate::paths::rig_config("beta").unwrap().exists());
    assert!(!crate::paths::rig_config("alpha").unwrap().exists());
}

#[test]
fn install_multi_rig_package_can_install_all() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));

    let result = install(package.path().to_str().unwrap(), None, true).expect("install");
    assert_eq!(result.installed.len(), 2);
    assert!(crate::paths::rig_config("alpha").unwrap().exists());
    assert!(crate::paths::rig_config("beta").unwrap().exists());
}

#[test]
fn install_rejects_existing_rig_collision() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    install(package.path().to_str().unwrap(), None, false).expect("first install");
    let err = install(package.path().to_str().unwrap(), None, false).expect_err("collision");
    assert!(err.message.contains("already exists"));
}

#[test]
fn installed_filename_is_runtime_identity_when_declared_id_differs() {
    let _home = HomeGuard::new();
    fs::create_dir_all(crate::paths::rigs().expect("rigs dir")).expect("rigs dir");
    fs::write(
        crate::paths::rig_config("replacement").expect("replacement rig path"),
        minimal_rig("alpha"),
    )
    .expect("replacement rig");

    let ids = list_ids().expect("list ids");
    assert_eq!(ids, vec!["replacement"]);

    let rigs = list().expect("list rigs");
    assert_eq!(rigs.len(), 1);
    assert_eq!(rigs[0].id, "replacement");
    assert_eq!(
        declared_id("replacement").expect("declared id"),
        Some("alpha".to_string())
    );

    assert_eq!(
        load("replacement").expect("load replacement").id,
        "replacement"
    );

    let err = load("alpha").expect_err("alpha should not resolve");
    assert_eq!(err.message, "Rig not found");
    assert_eq!(err.details["id"], "alpha");
    let hints = err
        .hints
        .iter()
        .map(|hint| hint.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(hints.contains("Did you mean: replacement?"));
    assert!(!hints.contains("Did you mean: alpha?"));
}

#[test]
fn git_url_installs_clone_package_and_config_link() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(package.path())
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(package.path())
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(package.path())
        .output()
        .expect("git commit");

    let bare = tempfile::tempdir().expect("bare parent");
    let source_path = bare.path().join("rig-package.git");
    let clone_bare = std::process::Command::new("git")
        .args([
            "clone",
            "--bare",
            package.path().to_str().unwrap(),
            source_path.to_str().unwrap(),
        ])
        .output()
        .expect("git clone --bare");
    assert!(clone_bare.status.success());

    let source = source_path.to_string_lossy().to_string();
    let result = install(&source, None, false).expect("install");
    assert!(!result.linked);
    assert_eq!(result.installed.len(), 1);
    assert!(result
        .package_path
        .parent()
        .unwrap()
        .ends_with("rig-packages"));
    assert!(crate::paths::rig_config("alpha").unwrap().exists());
    assert_eq!(read_source_metadata("alpha").unwrap().source, source);
}

#[test]
fn git_url_subpath_installs_single_rig_directory() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let subdir = package.path().join("packages").join("studio");
    write_single_rig(
        &subdir,
        "studio",
        include_str!("../../fixtures/rig-package-subpath/packages/studio/rig.json"),
    );
    write_rig(package.path(), "other", &minimal_rig("other"));
    let bare = bare_package(package.path());

    let root_source = bare.path().join("rig-package.git");
    let source = format!("{}//packages/studio", root_source.to_string_lossy());
    let result = install(&source, None, false).expect("install subpath");

    assert!(!result.linked);
    assert_eq!(result.source, root_source.to_string_lossy());
    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].id, "studio");
    assert!(result.installed[0]
        .spec_path
        .ends_with("packages/studio/rig.json"));
    assert!(crate::paths::rig_config("studio").unwrap().exists());
    assert!(!crate::paths::rig_config("other").unwrap().exists());

    let metadata = read_source_metadata("studio").expect("metadata");
    assert_eq!(metadata.source, root_source.to_string_lossy());
    assert!(metadata.rig_path.ends_with("packages/studio/rig.json"));
}

#[test]
fn git_url_subpath_preserves_multi_rig_ambiguity() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let nested = package.path().join("nested");
    write_rig(&nested, "alpha", &minimal_rig("alpha"));
    write_rig(&nested, "beta", &minimal_rig("beta"));
    let bare = bare_package(package.path());

    let source = format!(
        "{}//nested",
        bare.path().join("rig-package.git").to_string_lossy()
    );
    let err = install(&source, None, false).expect_err("ambiguous subpath");

    assert!(err.message.contains("multiple rigs"));
    assert!(err.message.contains("alpha"));
    assert!(err.message.contains("beta"));
}

#[test]
fn git_url_subpath_rejects_invalid_relative_path() {
    let _home = HomeGuard::new();
    let err = install("https://example.com/rigs.git//../secrets", None, false)
        .expect_err("invalid subpath");

    assert!(err.message.contains("non-empty relative path"));
}
