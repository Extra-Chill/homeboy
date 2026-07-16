#![cfg(test)]

use super::super::{
    lab_runtime_overlay_metadata, parse_runtime_overlays, runtime_overlay_env_overrides,
    runtime_overlay_install_workdir, sync_lab_runtime_overlays, LabWorkspaceMappingEntry,
    RuntimeOverlayInstallStep, RuntimeOverlaySpec, SyncedRuntimeOverlay,
    LAB_RUNTIME_OVERLAY_SCHEMA,
};

fn synced(role: &str, remote: &str, env: Option<&str>) -> SyncedRuntimeOverlay {
    SyncedRuntimeOverlay {
        role: role.to_string(),
        local_path: format!("/local/{role}"),
        remote_path: remote.to_string(),
        install_workdir: None,
        install_ran: false,
        expose_remote_path_env: env.map(str::to_string),
        build_provenance:
            crate::core::runner::runtime_overlay_freshness::RuntimeOverlayBuildProvenance::unverifiable(),
    }
}

#[test]
fn parses_artifact_only_overlay_without_install_step() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = RuntimeOverlaySpec {
        path: dir.path().display().to_string(),
        role: None,
        snapshot_includes: vec!["cli".to_string()],
        install: None,
        expose_remote_path_env: None,
    };

    let overlays = parse_runtime_overlays(vec![spec]).expect("parse overlays");

    assert_eq!(overlays.len(), 1);
    let overlay = &overlays[0];
    // Default role is applied and the artifact path is canonicalized to the
    // existing directory; no install step and no env surfacing requested.
    assert_eq!(overlay.workspace.role, "runtime_overlay");
    assert_eq!(overlay.workspace.snapshot_includes, vec!["cli".to_string()]);
    assert!(overlay.install.is_none());
    assert!(overlay.expose_remote_path_env.is_none());
    assert!(!overlay.workspace.bootstrap_node_dependencies);
}

#[test]
fn parses_overlay_with_opaque_install_step_and_env_surfacing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = RuntimeOverlaySpec {
        path: dir.path().display().to_string(),
        role: Some("cli-runtime".to_string()),
        snapshot_includes: Vec::new(),
        install: Some(RuntimeOverlayInstallStep {
            // Opaque, ecosystem-agnostic placeholder argv supplied as data.
            command: vec!["install-tool".to_string(), "deps".to_string()],
            workdir: Some("cli".to_string()),
        }),
        expose_remote_path_env: Some("RUNTIME_CLI_DIR".to_string()),
    };

    let overlays = parse_runtime_overlays(vec![spec]).expect("parse overlays");

    let overlay = &overlays[0];
    assert_eq!(overlay.workspace.role, "cli-runtime");
    let install = overlay.install.as_ref().expect("install step");
    assert_eq!(install.command, vec!["install-tool", "deps"]);
    assert_eq!(install.workdir.as_deref(), Some("cli"));
    assert_eq!(
        overlay.expose_remote_path_env.as_deref(),
        Some("RUNTIME_CLI_DIR")
    );
}

#[test]
fn rejects_install_step_with_empty_command_argv() {
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = RuntimeOverlaySpec {
        path: dir.path().display().to_string(),
        role: None,
        snapshot_includes: Vec::new(),
        install: Some(RuntimeOverlayInstallStep {
            command: vec!["   ".to_string()],
            workdir: None,
        }),
        expose_remote_path_env: None,
    };

    let err = parse_runtime_overlays(vec![spec]).expect_err("empty command rejected");
    assert!(err.message.contains("non-empty command argv"));
}

#[test]
fn rejects_overlay_with_missing_artifact_directory() {
    let spec = RuntimeOverlaySpec {
        path: "/definitely/not/a/real/overlay/dir".to_string(),
        role: None,
        snapshot_includes: Vec::new(),
        install: None,
        expose_remote_path_env: None,
    };

    assert!(parse_runtime_overlays(vec![spec]).is_err());
}

#[test]
fn install_workdir_defaults_to_overlay_remote_path() {
    assert_eq!(
        runtime_overlay_install_workdir("/srv/_lab/overlay", None),
        "/srv/_lab/overlay"
    );
    assert_eq!(
        runtime_overlay_install_workdir("/srv/_lab/overlay", Some("  ")),
        "/srv/_lab/overlay"
    );
}

#[test]
fn install_workdir_resolves_relative_against_remote_and_honors_absolute() {
    assert_eq!(
        runtime_overlay_install_workdir("/srv/_lab/overlay", Some("cli")),
        "/srv/_lab/overlay/cli"
    );
    assert_eq!(
        runtime_overlay_install_workdir("/srv/_lab/overlay", Some("/srv/_lab/sibling")),
        "/srv/_lab/sibling"
    );
}

#[test]
fn env_overrides_only_surface_overlays_that_declared_an_env_var() {
    let overlays = vec![
        synced("cli", "/srv/_lab/cli", Some("RUNTIME_CLI_DIR")),
        synced("data", "/srv/_lab/data", None),
    ];

    let overrides = runtime_overlay_env_overrides(&overlays);

    assert_eq!(
        overrides,
        vec![("RUNTIME_CLI_DIR".to_string(), "/srv/_lab/cli".to_string())]
    );
}

#[test]
fn env_overrides_empty_when_no_overlays() {
    assert!(runtime_overlay_env_overrides(&[]).is_empty());
}

#[test]
fn metadata_records_schema_count_and_overlays() {
    let overlays = vec![synced("cli", "/srv/_lab/cli", Some("RUNTIME_CLI_DIR"))];

    let value = lab_runtime_overlay_metadata(&overlays);

    assert_eq!(value["schema"], LAB_RUNTIME_OVERLAY_SCHEMA);
    assert_eq!(value["count"], 1);
    assert_eq!(value["overlays"][0]["role"], "cli");
    assert_eq!(value["overlays"][0]["remote_path"], "/srv/_lab/cli");
    assert_eq!(
        value["overlays"][0]["expose_remote_path_env"],
        "RUNTIME_CLI_DIR"
    );
}

#[test]
fn empty_overlay_list_is_a_no_op_and_leaves_mapping_unchanged() {
    // Components WITHOUT overlays must not sync anything or mutate the
    // workspace mapping — sync_lab_runtime_overlays short-circuits before
    // touching the runner, so this is safe to call without one.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut mapping: Vec<LabWorkspaceMappingEntry> = Vec::new();

    let synced = sync_lab_runtime_overlays(
        "unused-runner",
        &dir.path().display().to_string(),
        Vec::new(),
        &mut mapping,
    )
    .expect("no-op overlay sync");

    assert!(synced.is_empty());
    assert!(mapping.is_empty());
}
