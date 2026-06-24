//! Rig install lifecycle tests. Covers `src/core/rig/install.rs`.

use crate::core::rig::{
    declared_id, discover_rigs, install, list, list_ids, load, load_local_source,
    read_source_metadata, read_stack_source_metadata, run_check,
};
use crate::test_support::HomeGuard;
use std::fs;
use std::path::Path;

#[path = "support.rs"]
mod support;

use support::{minimal_rig, minimal_stack, write_rig, write_stack, GitFixture};

fn write_single_rig(dir: &Path, id: &str, body: &str) -> std::path::PathBuf {
    fs::create_dir_all(dir).expect("single rig dir");
    let rig_path = dir.join("rig.json");
    fs::write(&rig_path, body).expect("rig json");
    assert!(body.contains(id));
    rig_path
}

fn bare_package(package: &Path) -> tempfile::TempDir {
    let git = GitFixture::init(package);
    git.commit("update rigs");
    git.clone_bare()
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

    let installed = crate::core::paths::rig_config("alpha").expect("rig path");
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
fn install_package_with_sibling_stacks_installs_stack_specs() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "studio", &minimal_rig("studio"));
    let stack_path = write_stack(package.path(), "studio-combined", "studio");

    let result = install(package.path().to_str().unwrap(), None, false).expect("install");

    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed_stacks.len(), 1);
    assert_eq!(result.installed_stacks[0].id, "studio-combined");
    let installed = crate::core::paths::stack_config("studio-combined").expect("stack path");
    assert!(installed.exists());
    #[cfg(unix)]
    assert_eq!(fs::read_link(&installed).expect("symlink"), stack_path);
    assert_eq!(crate::core::stack::list().expect("stack list").len(), 1);
    assert_eq!(
        crate::core::stack::load("studio-combined")
            .expect("load stack")
            .component,
        "studio"
    );
    let metadata = read_stack_source_metadata("studio-combined").expect("stack metadata");
    assert!(metadata.linked);
    assert_eq!(metadata.stack_path, stack_path.to_string_lossy());
}

#[test]
fn install_git_source_subpath_records_nested_package_root() {
    let _home = HomeGuard::new();
    let repo = tempfile::tempdir().expect("repo");
    let nested = repo.path().join("woocommerce/woocommerce-gateway-stripe");
    fs::create_dir_all(nested.join("bench")).expect("bench dir");
    fs::write(
        nested.join("bench/stripe.trace.mjs"),
        "export default {};\n",
    )
    .expect("trace workload");
    write_rig(
        &nested,
        "woocommerce-stripe-ece-product-page",
        r#"{
            "id": "woocommerce-stripe-ece-product-page",
            "trace_workloads": {
                "woocommerce-gateway-stripe": [
                    { "path": "${package.root}/bench/stripe.trace.mjs" }
                ]
            }
        }"#,
    );
    let bare = bare_package(repo.path());
    let source = format!(
        "{}//woocommerce/woocommerce-gateway-stripe",
        bare.path().join("rig-package.git").display()
    );

    let result = install(&source, Some("woocommerce-stripe-ece-product-page"), false)
        .expect("install nested git package");
    let metadata = read_source_metadata("woocommerce-stripe-ece-product-page").expect("metadata");
    let source_root = result.source_root;
    let package_root = source_root.join("woocommerce/woocommerce-gateway-stripe");

    assert_eq!(result.package_path, package_root);
    assert_eq!(
        metadata.source_root.as_deref(),
        Some(source_root.to_str().unwrap())
    );
    assert_eq!(metadata.package_path, package_root.to_string_lossy());
    assert_eq!(
        metadata.discovery_path.as_deref(),
        Some(metadata.package_path.as_str())
    );

    let rig = load("woocommerce-stripe-ece-product-page").expect("load rig");
    assert_eq!(
        crate::core::rig::expand::expand_vars(&rig, "${package.root}/bench/stripe.trace.mjs"),
        package_root
            .join("bench/stripe.trace.mjs")
            .to_string_lossy()
    );
}

#[test]
fn install_git_source_missing_subpath_reports_source_and_package_roots() {
    let _home = HomeGuard::new();
    let repo = tempfile::tempdir().expect("repo");
    write_rig(repo.path(), "alpha", &minimal_rig("alpha"));
    let bare = bare_package(repo.path());
    let source = format!(
        "{}//woocommerce/woocommerce-gateway-stripe",
        bare.path().join("rig-package.git").display()
    );

    let err = install(&source, Some("alpha"), false).expect_err("missing nested package");

    assert!(err.message.contains("source root:"));
    assert!(err.message.contains("resolved package root:"));
    assert!(err
        .message
        .contains("woocommerce/woocommerce-gateway-stripe"));
}

#[test]
fn installed_rig_check_reports_package_lint_failures() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "lint-fixture", &minimal_rig("lint-fixture"));
    fs::write(package.path().join("conflicted.txt"), "<<<<<<< ours\n").expect("conflict fixture");

    install(package.path().to_str().unwrap(), None, false).expect("install");
    let rig = load("lint-fixture").expect("load rig");
    let report = run_check(&rig).expect("check report");

    assert!(
        !report.success,
        "package conflict marker should fail rig check"
    );
    assert!(report.pipeline.steps.iter().any(|step| {
        step.kind == "rig-package-lint"
            && step.status == "fail"
            && step
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("conflicted.txt")
    }));
}

#[test]
fn local_package_rig_check_loads_without_installing_source() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    fs::write(package.path().join("marker.txt"), "ok\n").expect("marker");
    write_rig(
        package.path(),
        "local-alpha",
        r#"{
            "id": "local-alpha",
            "pipeline": {
                "check": [
                    { "kind": "check", "label": "marker exists", "file": "${package.root}/marker.txt" }
                ]
            }
        }"#,
    );

    let rig = load_local_source(package.path().to_str().unwrap(), Some("local-alpha"))
        .expect("load local package rig");
    let report = run_check(&rig).expect("check local package rig");

    assert!(report.success, "local package check should pass");
    assert!(read_source_metadata("local-alpha").is_none());
    assert!(load("local-alpha").is_err(), "rig should not be installed");
}

#[test]
fn local_direct_rig_json_check_resolves_package_root() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    fs::write(package.path().join("marker.txt"), "ok\n").expect("marker");
    let rig_path = write_rig(
        package.path(),
        "direct-alpha",
        r#"{
            "id": "direct-alpha",
            "pipeline": {
                "check": [
                    { "kind": "check", "label": "marker exists", "file": "${package.root}/marker.txt" }
                ]
            }
        }"#,
    );

    let rig = load_local_source(rig_path.to_str().unwrap(), None).expect("load direct rig json");
    let report = run_check(&rig).expect("check direct rig json");

    assert!(report.success, "direct rig.json check should pass");
    assert_eq!(
        crate::core::rig::expand::expand_vars(&rig, "${package.root}/marker.txt"),
        package.path().join("marker.txt").to_string_lossy()
    );
    assert!(read_source_metadata("direct-alpha").is_none());
}

#[test]
fn install_materializes_rig_template_extends() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    fs::create_dir_all(package.path().join("templates")).expect("templates dir");
    fs::write(
        package.path().join("templates/base.json"),
        r#"{
            "description": "base rig",
            "components": {
                "app": { "path": "${env.DEV_ROOT}/base", "branch": "main" }
            },
            "pipeline": {
                "check": [
                    { "kind": "check", "label": "app exists", "file": "${components.app.path}" }
                ]
            }
        }"#,
    )
    .expect("base template");
    write_rig(
        package.path(),
        "alpha",
        r#"{
            "extends": "../../templates/base.json",
            "id": "alpha",
            "description": "alpha rig",
            "components": {
                "app": { "path": "${env.DEV_ROOT}/alpha" }
            }
        }"#,
    );

    let result = install(package.path().to_str().unwrap(), None, false).expect("install");

    assert_eq!(result.installed.len(), 1);
    let installed_path = crate::core::paths::rig_config("alpha").expect("rig config");
    #[cfg(unix)]
    assert!(fs::read_link(&installed_path).is_err());
    let installed = fs::read_to_string(&installed_path).expect("installed rig");
    assert!(!installed.contains("extends"));
    let rig = load("alpha").expect("load rig");
    assert_eq!(rig.description, "alpha rig");
    assert_eq!(rig.components["app"].path, "${env.DEV_ROOT}/alpha");
    assert_eq!(rig.components["app"].branch.as_deref(), Some("main"));
    assert_eq!(rig.pipeline["check"].len(), 1);
    assert!(
        read_source_metadata("alpha")
            .expect("metadata")
            .materialized
    );
}

#[test]
fn install_rejects_rig_template_extends_outside_source_root() {
    let _home = HomeGuard::new();
    let root = tempfile::tempdir().expect("root");
    let package = root.path().join("package");
    let outside = root.path().join("outside");
    fs::create_dir_all(&package).expect("package dir");
    fs::create_dir_all(&outside).expect("outside dir");
    fs::write(outside.join("base.json"), minimal_rig("base")).expect("outside base");
    fs::write(
        package.join("rig.json"),
        r#"{"id":"alpha","extends":"../outside/base.json"}"#,
    )
    .expect("rig json");

    let err = install(package.to_str().unwrap(), None, false).expect_err("outside extends");

    assert_eq!(err.details["field"], "extends");
    assert!(err.message.contains("inside the rig package source root"));
}

#[test]
fn installed_rig_load_applies_trace_workload_defaults() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(
        package.path(),
        "trace-defaults",
        r#"{
            "id": "trace-defaults",
            "trace_workload_defaults": {
                "trace-runner": {
                    "check_groups": [],
                    "port_range_size": 1,
                    "trace_default_phase_preset": "browser-waterfall",
                    "trace_phase_presets": {
                        "browser-waterfall": ["start:scenario.start", "done:scenario.done"]
                    },
                    "trace_span_metadata": {
                        "phase.start_to_done": { "category": "runtime_setup", "blocking": true }
                    }
                }
            },
            "trace_workloads": {
                "trace-runner": [
                    { "path": "${package.root}/bench/a.trace.mjs" },
                    { "path": "${package.root}/bench/b.trace.mjs", "port_range_size": 2 }
                ]
            }
        }"#,
    );

    install(package.path().to_str().unwrap(), None, false).expect("install");
    let rig = load("trace-defaults").expect("load rig");
    let workloads = rig
        .trace_workloads
        .get("trace-runner")
        .expect("trace workloads");

    assert_eq!(workloads[0].check_groups(), Some([].as_slice()));
    assert_eq!(workloads[0].port_range_size(), Some(1));
    assert_eq!(workloads[1].port_range_size(), Some(2));
    assert_eq!(
        workloads[0].trace_default_phase_preset(),
        Some("browser-waterfall")
    );
    assert!(workloads[0]
        .trace_phase_preset("browser-waterfall")
        .is_some());
    assert!(workloads[0]
        .trace_span_metadata()
        .contains_key("phase.start_to_done"));
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
    assert!(crate::core::paths::rig_config("beta").unwrap().exists());
    assert!(!crate::core::paths::rig_config("alpha").unwrap().exists());
}

#[test]
#[cfg(unix)]
fn install_id_skips_stacks_from_stale_unrelated_installed_sources() {
    let _home = HomeGuard::new();
    let stale_source = tempfile::tempdir().expect("stale source");
    write_rig(stale_source.path(), "stale-rig", &minimal_rig("stale-rig"));
    let stale_stack = write_stack(stale_source.path(), "stale-stack", "stale-component");
    install(stale_source.path().to_str().unwrap(), None, false).expect("install stale source");
    fs::remove_dir_all(stale_source.path()).expect("remove stale source");

    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));
    write_stack(package.path(), "stale-stack", "other-component");

    let result = install(package.path().to_str().unwrap(), Some("alpha"), false)
        .expect("install requested rig despite stale unrelated stack source");

    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].id, "alpha");
    assert!(result.installed_stacks.is_empty());
    assert!(crate::core::paths::rig_config("alpha").unwrap().exists());
    assert!(!crate::core::paths::rig_config("beta").unwrap().exists());
    assert_eq!(
        fs::read_link(crate::core::paths::stack_config("stale-stack").unwrap())
            .expect("stale stack symlink remains reported by sources list"),
        stale_stack
    );
}

#[test]
fn install_id_skips_invalid_unrelated_rigs_in_package() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    let broken = package.path().join("rigs").join("broken");
    fs::create_dir_all(&broken).expect("broken rig dir");
    fs::write(
        broken.join("rig.json"),
        r#"{"id":"broken","bench_workloads":{"node":["legacy"]}}"#,
    )
    .expect("broken rig json");

    let result = install(package.path().to_str().unwrap(), Some("alpha"), false).expect("install");

    assert_eq!(result.installed.len(), 1);
    assert_eq!(result.installed[0].id, "alpha");
    assert!(crate::core::paths::rig_config("alpha").unwrap().exists());
    assert!(!crate::core::paths::rig_config("broken").unwrap().exists());
}

#[test]
fn install_id_missing_reports_requested_rig_not_found() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    let err = install(package.path().to_str().unwrap(), Some("missing"), false)
        .expect_err("missing rig should error");

    assert_eq!(err.details["field"], "id");
    assert!(err.message.contains("missing"));
    assert!(err.message.contains("not found"));
}

#[test]
fn install_multi_rig_package_can_install_all() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));
    write_rig(package.path(), "beta", &minimal_rig("beta"));

    let result = install(package.path().to_str().unwrap(), None, true).expect("install");
    assert_eq!(result.installed.len(), 2);
    assert!(crate::core::paths::rig_config("alpha").unwrap().exists());
    assert!(crate::core::paths::rig_config("beta").unwrap().exists());
}

#[test]
fn install_refreshes_existing_matching_local_rig_without_metadata() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let refreshed = minimal_rig("alpha").replace("alpha rig", "alpha rig refreshed");
    write_rig(package.path(), "alpha", &refreshed);

    fs::create_dir_all(crate::core::paths::rigs().expect("rigs dir")).expect("rigs dir");
    fs::write(
        crate::core::paths::rig_config("alpha").expect("alpha rig path"),
        minimal_rig("alpha"),
    )
    .expect("stale installed rig");

    let result = install(package.path().to_str().unwrap(), None, false).expect("refresh");

    assert_eq!(result.installed.len(), 1);
    let installed = fs::read_to_string(crate::core::paths::rig_config("alpha").unwrap())
        .expect("installed rig");
    assert!(installed.contains("alpha rig refreshed"));
    assert_eq!(
        read_source_metadata("alpha").expect("metadata").rig_path,
        package.path().join("rigs/alpha/rig.json").to_string_lossy()
    );
}

#[test]
#[cfg(unix)]
fn install_refreshes_existing_broken_rig_symlink() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let refreshed = minimal_rig("alpha").replace("alpha rig", "alpha rig refreshed");
    let source = write_rig(package.path(), "alpha", &refreshed);

    fs::create_dir_all(crate::core::paths::rigs().expect("rigs dir")).expect("rigs dir");
    let installed = crate::core::paths::rig_config("alpha").expect("alpha rig path");
    std::os::unix::fs::symlink(package.path().join("missing-rig.json"), &installed)
        .expect("broken rig symlink");

    let result = install(package.path().to_str().unwrap(), None, false).expect("refresh");

    assert_eq!(result.installed.len(), 1);
    assert_eq!(fs::read_link(&installed).expect("updated symlink"), source);
    let installed_content = fs::read_to_string(&installed).expect("installed rig");
    assert!(installed_content.contains("alpha rig refreshed"));
}

#[test]
fn install_rejects_existing_rig_with_different_declared_id() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    fs::create_dir_all(crate::core::paths::rigs().expect("rigs dir")).expect("rigs dir");
    fs::write(
        crate::core::paths::rig_config("alpha").expect("alpha rig path"),
        minimal_rig("beta"),
    )
    .expect("conflicting installed rig");

    let err = install(package.path().to_str().unwrap(), None, false).expect_err("collision");
    assert!(err.message.contains("refusing to replace"));
}

#[test]
fn install_refreshes_existing_matching_stack_without_metadata() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "studio", &minimal_rig("studio"));
    let stack_path = write_stack(package.path(), "studio-combined", "studio");

    fs::create_dir_all(crate::core::paths::stacks().expect("stacks dir")).expect("stacks dir");
    fs::write(
        crate::core::paths::stack_config("studio-combined").expect("stack path"),
        fs::read_to_string(&stack_path).expect("package stack"),
    )
    .expect("existing stack");

    let result = install(package.path().to_str().unwrap(), None, false).expect("refresh");

    assert_eq!(result.installed_stacks.len(), 1);
    assert_eq!(result.installed_stacks[0].id, "studio-combined");
    let metadata = read_stack_source_metadata("studio-combined").expect("stack metadata");
    assert_eq!(metadata.stack_path, stack_path.to_string_lossy());
}

#[test]
fn install_rejects_existing_stack_with_different_content() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "studio", &minimal_rig("studio"));
    write_stack(package.path(), "studio-combined", "studio");
    let manual_stack = minimal_stack("studio-combined", "other");

    fs::create_dir_all(crate::core::paths::stacks().expect("stacks dir")).expect("stacks dir");
    let stack_config = crate::core::paths::stack_config("studio-combined").expect("stack path");
    fs::write(&stack_config, &manual_stack).expect("conflicting stack");

    let err = install(package.path().to_str().unwrap(), None, false).expect_err("stack collision");
    assert!(err.message.contains("different content"));
    assert_eq!(
        fs::read_to_string(stack_config).expect("manual stack preserved"),
        manual_stack
    );
}

#[test]
fn installed_filename_is_runtime_identity_when_declared_id_differs() {
    let _home = HomeGuard::new();
    fs::create_dir_all(crate::core::paths::rigs().expect("rigs dir")).expect("rigs dir");
    fs::write(
        crate::core::paths::rig_config("replacement").expect("replacement rig path"),
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
fn rig_list_ids_skip_non_json_files() {
    let _home = HomeGuard::new();
    fs::create_dir_all(crate::core::paths::rigs().expect("rigs dir")).expect("rigs dir");
    fs::write(
        crate::core::paths::rig_config("alpha").expect("alpha rig path"),
        minimal_rig("alpha"),
    )
    .expect("alpha rig");
    fs::write(
        crate::core::paths::rigs()
            .expect("rigs dir")
            .join("ignore.txt"),
        "not json",
    )
    .expect("non-json rig sidecar");

    assert_eq!(list_ids().expect("list ids"), vec!["alpha"]);
    assert_eq!(list().expect("list rigs").len(), 1);
}

#[test]
fn git_url_installs_clone_package_and_config_link() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    write_rig(package.path(), "alpha", &minimal_rig("alpha"));

    let bare = bare_package(package.path());
    let source = support::bare_source_path(&bare)
        .to_string_lossy()
        .to_string();
    let result = install(&source, None, false).expect("install");
    assert!(!result.linked);
    assert_eq!(result.installed.len(), 1);
    assert!(result
        .package_path
        .parent()
        .unwrap()
        .ends_with("rig-packages"));
    assert!(crate::core::paths::rig_config("alpha").unwrap().exists());
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
    assert!(crate::core::paths::rig_config("studio").unwrap().exists());
    assert!(!crate::core::paths::rig_config("other").unwrap().exists());

    let metadata = read_source_metadata("studio").expect("metadata");
    assert_eq!(metadata.source, root_source.to_string_lossy());
    assert!(metadata.rig_path.ends_with("packages/studio/rig.json"));
}

#[test]
fn git_url_subpath_installs_only_stacks_under_selected_subpath() {
    let _home = HomeGuard::new();
    let package = tempfile::tempdir().expect("package");
    let selected = package.path().join("packages").join("studio");
    write_single_rig(&selected, "studio", &minimal_rig("studio"));
    write_stack(&selected, "studio-combined", "studio");
    write_stack(package.path(), "root-combined", "root");
    let bare = bare_package(package.path());
    let source = format!(
        "{}//packages/studio",
        bare.path().join("rig-package.git").to_string_lossy()
    );

    let result = install(&source, None, false).expect("install subpath");

    assert_eq!(result.installed_stacks.len(), 1);
    assert_eq!(result.installed_stacks[0].id, "studio-combined");
    assert!(crate::core::paths::stack_config("studio-combined")
        .unwrap()
        .exists());
    assert!(!crate::core::paths::stack_config("root-combined")
        .unwrap()
        .exists());
    let stacks = crate::core::stack::list().expect("stack list");
    assert_eq!(stacks.len(), 1);
    assert_eq!(stacks[0].id, "studio-combined");
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
