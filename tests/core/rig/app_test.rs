//! App launcher tests for `src/core/rig/app.rs`.

use std::collections::HashMap;
use std::fs;

use crate::app::{install_inner, uninstall_inner, AppLauncherAction, AppLauncherOptions};
use crate::spec::{
    AppLauncherPlatform, AppLauncherPreflight, AppLauncherSpec, ComponentSpec, RigSpec,
};

fn rig_with_launcher(install_dir: &str) -> RigSpec {
    let mut components = HashMap::new();
    components.insert(
        "studio".to_string(),
        ComponentSpec {
            path: "/tmp/studio-dev".to_string(),
            component_id: None,
            path_setting: None,
            checkout_root: None,
            remote_url: None,
            lab_stack: None,
            branch: None,
            r#ref: None,
            default_ref: None,
            extensions: None,
        },
    );

    RigSpec {
        effective_component_paths: Default::default(),
        workloads: Default::default(),
        id: "studio-dev".to_string(),
        description: String::new(),
        components,
        services: Default::default(),
        symlinks: Vec::new(),
        shared_paths: Vec::new(),
        resources: Default::default(),
        lifecycle: Default::default(),
        requirements: Default::default(),
        pipeline: Default::default(),
        bench: None,
        fuzz: None,
        trace: Default::default(),
        bench_workloads: Default::default(),
        trace_workloads: Default::default(),
        fuzz_workloads: Default::default(),
        trace_workload_defaults: Default::default(),
        trace_variants: Default::default(),
        trace_profiles: Default::default(),
        trace_experiments: Default::default(),
        trace_guardrails: Default::default(),
        bench_profiles: Default::default(),
        fuzz_profiles: Default::default(),
        app_launcher: Some(AppLauncherSpec {
            platform: AppLauncherPlatform::Macos,
            wrapper_display_name: "Studio (Dev)".to_string(),
            wrapper_bundle_id: "com.example.studio-dev".to_string(),
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
            "wrapper_bundle_id": "com.example.studio-dev",
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
    assert_eq!(launcher.wrapper_bundle_id, "com.example.studio-dev");
    assert_eq!(launcher.preflight, vec![AppLauncherPreflight::RigCheck]);
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
fn test_public_install_dry_run_previews_paths_on_any_host() {
    let cases: [(fn(&str) -> RigSpec, AppLauncherPlatform, &str, &str, usize); 2] = [
        (
            rig_with_launcher,
            AppLauncherPlatform::Macos,
            "Studio (Dev).app",
            "/tmp/studio-dev/out/Studio.app",
            3,
        ),
        (
            rig_with_linux_launcher,
            AppLauncherPlatform::Linux,
            "Studio (Dev).desktop",
            "/tmp/studio-dev/out/studio",
            1,
        ),
    ];

    for (make_rig, platform, file_name, target, file_count) in cases {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let rig = make_rig(&tmp.path().to_string_lossy());
        let report = crate::app::install(&rig, AppLauncherOptions { dry_run: true })
            .expect("dry-run preview");

        assert!(report.dry_run);
        assert_eq!(report.action, AppLauncherAction::Install);
        assert_eq!(report.platform, platform);
        assert_eq!(
            report.launcher_path,
            tmp.path().join(file_name).display().to_string()
        );
        assert_eq!(report.target_app, target);
        assert_eq!(report.files.len(), file_count, "{file_name}");
        assert!(!tmp.path().join(file_name).exists(), "{file_name}");
    }
}

#[test]
fn test_install_then_uninstall_round_trips_launcher_files() {
    let cases: [(fn(&str) -> RigSpec, &str, &str, &str); 2] = [
        (
            rig_with_launcher,
            "Studio (Dev).app",
            "Contents/MacOS/launch",
            "rig up 'studio-dev'",
        ),
        (
            rig_with_linux_launcher,
            "Studio (Dev).desktop",
            "",
            "Exec=/bin/sh -lc",
        ),
    ];

    for (make_rig, file_name, content_file, expected) in cases {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let rig = make_rig(&tmp.path().to_string_lossy());
        let launcher = tmp.path().join(file_name);

        let report =
            install_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("install");
        assert!(!report.dry_run);
        let content_path = if content_file.is_empty() {
            launcher.clone()
        } else {
            launcher.join(content_file)
        };
        assert!(fs::read_to_string(&content_path)
            .expect("read generated launcher")
            .contains(expected));

        let report =
            uninstall_inner(&rig, AppLauncherOptions { dry_run: false }, false).expect("uninstall");
        assert_eq!(report.action, AppLauncherAction::Uninstall);
        assert!(!launcher.exists(), "{file_name} removed");
    }
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
fn test_public_install_refuses_launchers_for_other_hosts() {
    let cases: [(fn(&str) -> RigSpec, bool, &str); 2] = [
        (
            rig_with_launcher,
            cfg!(target_os = "macos"),
            "macOS app launchers",
        ),
        (
            rig_with_linux_launcher,
            cfg!(target_os = "linux"),
            "Linux .desktop launchers",
        ),
    ];

    for (make_rig, host_matches, refusal) in cases {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let rig = make_rig(&tmp.path().to_string_lossy());
        let result = crate::app::install(&rig, AppLauncherOptions { dry_run: false });
        if host_matches {
            assert!(result.is_ok(), "{refusal} should install on matching host");
        } else {
            let err = result.expect_err("mismatched host refuses install");
            assert!(err.to_string().contains(refusal));
        }
    }
}
