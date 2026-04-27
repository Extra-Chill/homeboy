//! App launcher tests for `src/core/rig/app.rs`.

use std::collections::HashMap;
use std::fs;

use crate::rig::app::{install_inner, uninstall_inner, AppLauncherAction, AppLauncherOptions};
use crate::rig::spec::{
    AppLauncherPlatform, AppLauncherPreflight, AppLauncherSpec, ComponentSpec, RigSpec,
};

fn rig_with_launcher(install_dir: &str) -> RigSpec {
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        ComponentSpec {
            path: "/tmp/studio-dev".to_string(),
            remote_url: None,
            stack: None,
            branch: None,
        },
    );

    RigSpec {
        id: "studio-dev".to_string(),
        description: String::new(),
        components,
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        pipeline: Default::default(),
        bench: None,
        bench_workloads: Default::default(),
        app_launcher: Some(AppLauncherSpec {
            platform: AppLauncherPlatform::Macos,
            wrapper_display_name: "Studio (Dev)".to_string(),
            wrapper_bundle_id: "com.chubes.studio-dev".to_string(),
            target_app: "${components.studio.path}/out/Studio.app".to_string(),
            install_dir: Some(install_dir.to_string()),
            preflight: vec![AppLauncherPreflight::RigCheck],
            on_preflight_fail: Some("dialog-and-open-terminal".to_string()),
        }),
    }
}

#[test]
fn test_app_launcher_spec_parses() {
    let json = r#"{
        "id": "studio-dev",
        "components": { "studio": { "path": "/tmp/studio" } },
        "app_launcher": {
            "platform": "macos",
            "wrapper_display_name": "Studio (Dev)",
            "wrapper_bundle_id": "com.chubes.studio-dev",
            "target_app": "${components.studio.path}/out/Studio.app",
            "install_dir": "/tmp/apps",
            "preflight": ["rig:check"],
            "on_preflight_fail": "dialog-and-open-terminal"
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let launcher = spec.app_launcher.expect("launcher");
    assert_eq!(launcher.platform, AppLauncherPlatform::Macos);
    assert_eq!(launcher.wrapper_display_name, "Studio (Dev)");
    assert_eq!(launcher.wrapper_bundle_id, "com.chubes.studio-dev");
    assert_eq!(launcher.preflight, vec![AppLauncherPreflight::RigCheck]);
}

#[test]
fn test_resolve_launcher_expands_paths() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let launcher = super::resolve_launcher(&rig).expect("resolve");
    assert_eq!(launcher.target_path, "/tmp/studio-dev/out/Studio.app");
    assert!(launcher.launcher_path.ends_with("Studio (Dev).app"));
    assert_eq!(launcher.launcher_path.parent().unwrap(), tmp.path());
}

#[test]
fn test_generated_wrapper_content_runs_check_up_then_target() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let launcher = super::resolve_launcher(&rig).expect("resolve");
    let script = super::bundle::render_launcher_script(&rig, &launcher);
    assert!(script.starts_with("#!/bin/sh"));
    assert!(script.contains("HOMEBOY_BIN=\"${HOMEBOY_BIN:-homeboy}\""));
    assert!(script.contains("rig check 'studio-dev'"));
    assert!(script.contains("rig up 'studio-dev'"));
    assert!(script.contains("tell application \"Terminal\" to do script"));
    assert!(script.contains("TARGET_APP='/tmp/studio-dev/out/Studio.app'"));
    assert!(script.contains("exec open -n \"$TARGET_APP\" --args \"$@\""));
}

#[test]
fn test_install_dry_run_reports_paths_without_writing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let report = install_inner(&rig, AppLauncherOptions { dry_run: true }, false).expect("plan");
    assert!(report.dry_run);
    assert_eq!(report.action, AppLauncherAction::Install);
    assert!(report.launcher_path.ends_with("Studio (Dev).app"));
    assert_eq!(report.files.len(), 3);
    assert!(!tmp.path().join("Studio (Dev).app").exists());
}

#[test]
fn test_install_writes_script_backed_bundle_to_temp_dir() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let report =
        install_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("install");
    let app = tmp.path().join("Studio (Dev).app");
    let plist = app.join("Contents/Info.plist");
    let script = app.join("Contents/MacOS/launch");

    assert!(!report.dry_run);
    assert!(plist.exists(), "Info.plist written");
    assert!(script.exists(), "launch script written");
    assert!(fs::read_to_string(plist)
        .expect("read plist")
        .contains("com.chubes.studio-dev"));
    assert!(fs::read_to_string(script)
        .expect("read script")
        .contains("homeboy"));
}

#[test]
fn test_uninstall_removes_generated_bundle_from_temp_dir() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    install_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("install");
    let app = tmp.path().join("Studio (Dev).app");
    assert!(app.exists());

    let report =
        uninstall_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("uninstall");
    assert_eq!(report.action, AppLauncherAction::Uninstall);
    assert!(!app.exists(), "bundle removed");
}

#[test]
fn test_update_reports_update_action() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let report = crate::rig::app::update(&rig, AppLauncherOptions { dry_run: true })
        .expect("update dry-run");
    assert_eq!(report.action, AppLauncherAction::Update);
    assert!(report.dry_run);
}

#[test]
fn test_public_install_refuses_unsupported_platforms() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let result = crate::rig::app::install(&rig, AppLauncherOptions { dry_run: false });
    if cfg!(target_os = "macos") {
        assert!(result.is_ok(), "macOS should allow install");
    } else {
        let err = result.expect_err("non-macOS refuses install");
        assert!(err.to_string().contains("macOS app launchers"));
    }
}

#[test]
fn test_public_install_dry_run_is_cross_platform_preview() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let report = crate::rig::app::install(&rig, AppLauncherOptions { dry_run: true })
        .expect("dry-run preview");
    assert!(report.dry_run);
    assert!(!tmp.path().join("Studio (Dev).app").exists());
}
