use homeboy::core::deps::{self, DependencyUpdateOptions};
use homeboy::extensions::deps_provider;
use std::fs;
use tempfile::tempdir;

fn write_file(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

#[test]
fn status_reads_npm_direct_constraints_and_lock_details() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let root_path = root.display().to_string();

    write_file(
        &root.join("package.json"),
        r#"{
            "name": "fixture-root",
            "dependencies": {
                "fixture-prod": "^1.0.0"
            },
            "devDependencies": {
                "fixture-dev": "^2.0.0"
            }
        }"#,
    );
    write_file(
        &root.join("package-lock.json"),
        r#"{
            "name": "fixture-root",
            "lockfileVersion": 3,
            "packages": {
                "": {
                    "dependencies": { "fixture-prod": "^1.0.0" },
                    "devDependencies": { "fixture-dev": "^2.0.0" }
                },
                "node_modules/fixture-prod": {
                    "version": "1.2.3",
                    "resolved": "https://registry.npmjs.org/fixture-prod/-/fixture-prod-1.2.3.tgz"
                },
                "node_modules/fixture-transitive": {
                    "version": "0.1.0",
                    "integrity": "sha512-transitive"
                },
                "node_modules/fixture-dev": {
                    "version": "2.1.0",
                    "resolved": "https://registry.npmjs.org/fixture-dev/-/fixture-dev-2.1.0.tgz"
                }
            }
        }"#,
    );

    let status = deps::status(Some("fixture"), Some(&root_path), None).unwrap();

    assert_eq!(status.component_id, "fixture");
    assert_eq!(status.package_manager, "npm");
    assert_eq!(status.packages.len(), 3);

    let prod = status
        .packages
        .iter()
        .find(|package| package.name == "fixture-prod")
        .unwrap();
    assert_eq!(prod.manifest_section.as_deref(), Some("dependencies"));
    assert_eq!(prod.constraint.as_deref(), Some("^1.0.0"));
    assert_eq!(prod.locked_version.as_deref(), Some("1.2.3"));
    assert_eq!(
        prod.locked_reference.as_deref(),
        Some("https://registry.npmjs.org/fixture-prod/-/fixture-prod-1.2.3.tgz")
    );

    let dev = status
        .packages
        .iter()
        .find(|package| package.name == "fixture-dev")
        .unwrap();
    assert_eq!(dev.manifest_section.as_deref(), Some("devDependencies"));
    assert_eq!(dev.locked_version.as_deref(), Some("2.1.0"));

    let transitive = status
        .packages
        .iter()
        .find(|package| package.name == "fixture-transitive")
        .unwrap();
    assert_eq!(transitive.manifest_section, None);
    assert_eq!(transitive.constraint, None);
    assert_eq!(
        transitive.locked_reference.as_deref(),
        Some("sha512-transitive")
    );
}

#[test]
fn status_combines_composer_and_npm_providers_generically() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let root_path = root.display().to_string();

    write_file(
        &root.join("composer.json"),
        r#"{ "require": { "fixture/composer": "^1.0" } }"#,
    );
    write_file(
        &root.join("package.json"),
        r#"{ "dependencies": { "fixture-npm": "^2.0.0" } }"#,
    );

    let status = deps::status(Some("fixture"), Some(&root_path), None).unwrap();

    assert_eq!(status.package_manager, "composer,npm");
    assert!(status
        .packages
        .iter()
        .any(|package| package.name == "fixture/composer"));
    assert!(status
        .packages
        .iter()
        .any(|package| package.name == "fixture-npm"));
}

#[test]
fn test_npm_command_args() {
    assert_eq!(
        deps_provider::npm_command_args("fixture-package", Some("^2.0.0")),
        vec!["install", "fixture-package@^2.0.0"]
    );

    assert_eq!(
        deps_provider::npm_command_args("@fixture/package", Some("^2.0.0")),
        vec!["install", "@fixture/package@^2.0.0"]
    );

    assert_eq!(
        deps_provider::npm_command_args("fixture-package", None),
        vec!["update", "fixture-package"]
    );
}

#[test]
#[ignore = "integration test mutates real npm manifests/locks and shells out to npm"]
fn npm_update_with_constraint_changes_manifest_and_lock_for_local_path_package() {
    if std::process::Command::new("npm")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("npm not found; skipping integration-ish deps update test");
        return;
    }

    let dir = tempdir().unwrap();
    let root = dir.path().join("root");
    let package_v1 = dir.path().join("package-v1");
    let package_v2 = dir.path().join("package-v2");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&package_v1).unwrap();
    fs::create_dir_all(&package_v2).unwrap();

    write_file(
        &package_v1.join("package.json"),
        r#"{ "name": "fixture-package", "version": "1.0.0" }"#,
    );
    write_file(
        &package_v2.join("package.json"),
        r#"{ "name": "fixture-package", "version": "1.1.0" }"#,
    );
    write_file(
        &root.join("package.json"),
        r#"{
            "name": "fixture-root",
            "version": "1.0.0",
            "dependencies": { "fixture-package": "file:../package-v1" }
        }"#,
    );

    let initial = std::process::Command::new("npm")
        .args(["install", "--ignore-scripts"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        initial.status.success(),
        "initial npm install failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&initial.stdout),
        String::from_utf8_lossy(&initial.stderr)
    );

    let root_path = root.display().to_string();
    let result = deps::update(
        Some("fixture"),
        Some(&root_path),
        "fixture-package",
        Some("file:../package-v2"),
        DependencyUpdateOptions {
            install: false,
            rebuild: false,
        },
    )
    .unwrap();

    assert_eq!(result.package_manager, "npm");
    assert_eq!(
        result.before.unwrap().locked_version.as_deref(),
        Some("1.0.0")
    );
    let after = result.after.unwrap();
    assert_eq!(after.constraint.as_deref(), Some("file:../package-v2"));
    assert_eq!(after.locked_version.as_deref(), Some("1.1.0"));
    assert_eq!(
        result.command,
        vec!["npm", "install", "fixture-package@file:../package-v2"]
    );
}
