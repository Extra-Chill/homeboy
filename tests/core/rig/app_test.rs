//! App launcher tests for `src/core/rig/app.rs`.

use std::collections::HashMap;
use std::fs;

use crate::core::rig::app::{
    install_inner, uninstall_inner, AppLauncherAction, AppLauncherOptions,
};
use crate::core::rig::spec::{
    AppLauncherPlatform, AppLauncherPreflight, AppLauncherSpec, ComponentSpec, RigSpec,
};

fn rig_with_launcher(install_dir: &str) -> RigSpec {
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        ComponentSpec {
            path: "/tmp/studio-dev".to_string(),
            checkout_root: None,
            remote_url: None,
            triage_remote_url: None,
            stack: None,
            branch: None,
            r#ref: None,
            extensions: None,
        },
    );

    RigSpec {
        id: "studio-dev".to_string(),
        description: String::new(),
        components,
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        pipeline: Default::default(),
        bench: None,
        bench_workloads: Default::default(),
        trace_workloads: Default::default(),
        trace_workload_defaults: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
        bench_profiles: Default::default(),
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

fn rig_with_linux_launcher(install_dir: &str) -> RigSpec {
    let mut rig = rig_with_launcher(install_dir);
    let launcher = rig.app_launcher.as_mut().expect("launcher");
    launcher.platform = AppLauncherPlatform::Linux;
    launcher.target_app = "${components.studio.path}/out/studio".to_string();
    rig
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
fn test_linux_app_launcher_spec_parses() {
    let json = r#"{
        "id": "studio-dev",
        "components": { "studio": { "path": "/tmp/studio" } },
        "app_launcher": {
            "platform": "linux",
            "wrapper_display_name": "Studio Dev",
            "wrapper_bundle_id": "com.chubes.studio-dev",
            "target_app": "${components.studio.path}/out/studio",
            "install_dir": "/tmp/apps",
            "preflight": ["rig:check"]
        }
    }"#;
    let spec: RigSpec = serde_json::from_str(json).expect("parse");
    let launcher = spec.app_launcher.expect("launcher");
    assert_eq!(launcher.platform, AppLauncherPlatform::Linux);
    assert_eq!(launcher.wrapper_display_name, "Studio Dev");
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
fn test_resolve_linux_launcher_uses_desktop_extension() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_linux_launcher(&tmp.path().to_string_lossy());
    let launcher = super::resolve_launcher(&rig).expect("resolve");
    assert_eq!(launcher.target_path, "/tmp/studio-dev/out/studio");
    assert!(launcher.launcher_path.ends_with("Studio (Dev).desktop"));
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
fn test_generated_linux_desktop_entry_runs_check_up_then_target() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_linux_launcher(&tmp.path().to_string_lossy());
    let launcher = super::resolve_launcher(&rig).expect("resolve");
    let desktop = super::bundle::render_desktop_entry(&rig, &launcher);
    assert!(desktop.starts_with("[Desktop Entry]"));
    assert!(desktop.contains("Name=Studio (Dev)"));
    assert!(desktop.contains("HOMEBOY_BIN=\"${HOMEBOY_BIN:-homeboy}\""));
    assert!(desktop.contains("rig check"));
    assert!(desktop.contains("rig up"));
    assert!(desktop.contains("studio-dev"));
    assert!(desktop.contains("/tmp/studio-dev/out/studio"));
    assert!(desktop.contains("\"$@\""));
    assert!(desktop.contains("homeboy-launcher %U"));
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
fn test_linux_install_dry_run_reports_desktop_file_without_writing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_linux_launcher(&tmp.path().to_string_lossy());
    let report = install_inner(&rig, AppLauncherOptions { dry_run: true }, false).expect("plan");
    assert!(report.dry_run);
    assert_eq!(report.platform, AppLauncherPlatform::Linux);
    assert!(report.launcher_path.ends_with("Studio (Dev).desktop"));
    assert_eq!(report.files.len(), 1);
    assert!(!tmp.path().join("Studio (Dev).desktop").exists());
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
fn test_linux_install_writes_desktop_file_to_temp_dir() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_linux_launcher(&tmp.path().to_string_lossy());
    let report =
        install_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("install");
    let desktop = tmp.path().join("Studio (Dev).desktop");

    assert!(!report.dry_run);
    assert!(desktop.exists(), "desktop file written");
    assert!(fs::read_to_string(desktop)
        .expect("read desktop")
        .contains("Exec=/bin/sh -lc"));
}

#[test]
fn test_linux_update_regenerates_desktop_file_in_place() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_linux_launcher(&tmp.path().to_string_lossy());
    install_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("install");
    let desktop = tmp.path().join("Studio (Dev).desktop");
    fs::write(&desktop, "stale desktop entry").expect("write stale desktop");

    let report =
        super::update_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("update");
    let content = fs::read_to_string(&desktop).expect("read desktop");

    assert_eq!(report.action, AppLauncherAction::Update);
    assert_eq!(report.files, vec![desktop.display().to_string()]);
    assert!(content.starts_with("[Desktop Entry]"));
    assert!(content.contains("Exec=/bin/sh -lc"));
    assert!(!content.contains("stale desktop entry"));
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
fn test_uninstall_removes_generated_linux_desktop_file_from_temp_dir() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_linux_launcher(&tmp.path().to_string_lossy());
    install_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("install");
    let desktop = tmp.path().join("Studio (Dev).desktop");
    assert!(desktop.exists());

    let report =
        uninstall_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("uninstall");
    assert_eq!(report.action, AppLauncherAction::Uninstall);
    assert!(!desktop.exists(), "desktop file removed");
}

#[test]
fn test_public_linux_install_refuses_on_non_linux_hosts() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_linux_launcher(&tmp.path().to_string_lossy());
    let result = crate::core::rig::app::install(&rig, AppLauncherOptions { dry_run: false });
    if cfg!(target_os = "linux") {
        assert!(result.is_ok(), "Linux should allow install");
    } else {
        let err = result.expect_err("non-Linux refuses install");
        assert!(err.to_string().contains("Linux .desktop launchers"));
    }
}

#[test]
fn test_update_reports_update_action() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let report = crate::core::rig::app::update(&rig, AppLauncherOptions { dry_run: true })
        .expect("update dry-run");
    assert_eq!(report.action, AppLauncherAction::Update);
    assert!(report.dry_run);
}

#[test]
fn test_public_install_refuses_unsupported_platforms() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let rig = rig_with_launcher(&tmp.path().to_string_lossy());
    let result = crate::core::rig::app::install(&rig, AppLauncherOptions { dry_run: false });
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
    let report = crate::core::rig::app::install(&rig, AppLauncherOptions { dry_run: true })
        .expect("dry-run preview");
    assert!(report.dry_run);
    assert!(!tmp.path().join("Studio (Dev).app").exists());
}
