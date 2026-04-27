//! Rig install lifecycle tests. Covers `src/core/rig/install.rs`.

use crate::rig::{discover_rigs, install, read_source_metadata};
use crate::test_support::HomeGuard;
use std::fs;
use std::path::Path;

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
