//! Daemon file-API tests that require the real runner-workspace-root provider.
//!
//! Core's daemon `/files/*` routes resolve a request path against the runner's
//! configured `workspace_root` through the `RunnerWorkspaceRootProvider` hook,
//! whose real implementation lives in this runner crate (extracted from core in
//! Extra-Chill/homeboy#8698). These tests were moved out of `homeboy-core`'s
//! daemon tests because they need that provider to resolve a runner's workspace
//! root — which a core-only stub cannot do without becoming a test double. Here
//! the real provider is registered, so path containment is enforced against the
//! runner's actual configured root.

use base64::Engine;
use homeboy_core::api_jobs::JobStore;
use homeboy_core::daemon::route_with_body;
use homeboy_core::test_support::HomeGuard;

/// Register the runner-side workspace-root provider the daemon file API resolves
/// runner roots through. Production wires this at CLI startup; the registration
/// is an idempotent process-global slot, so registering per test is safe.
fn register_provider() {
    crate::register_runner_workspace_root_provider();
}

fn write_runner_config(id: &str, value: &serde_json::Value) {
    let dir = homeboy_core::paths::homeboy()
        .expect("homeboy dir")
        .join("runners");
    std::fs::create_dir_all(&dir).expect("create runners dir");
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(value).expect("serialize runner"),
    )
    .expect("write runner config");
}

fn file_api_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    write_runner_config(
        "file-lab",
        &serde_json::json!({
            "id": "file-lab",
            "kind": "local",
            "workspace_root": workspace.display().to_string(),
        }),
    );
    (temp, workspace)
}

#[test]
fn file_route_rejects_paths_outside_runner_workspace_root() {
    register_provider();
    let _home = HomeGuard::new();
    let (_temp, workspace) = file_api_workspace();

    let response = route_with_body(
        "POST",
        "/files/download",
        Some(serde_json::json!({
            "runner_id": "file-lab",
            "path": workspace.join("..").join("secret.txt").display().to_string(),
        })),
        &JobStore::default(),
    );

    assert_eq!(response.status_code, 400);
    assert_eq!(response.body["details"]["field"], "path");
    assert!(response.body["message"]
        .as_str()
        .expect("message")
        .contains("workspace_root"));
}

#[test]
fn file_routes_upload_and_download_inside_runner_workspace_root() {
    register_provider();
    let _home = HomeGuard::new();
    let (_temp, workspace) = file_api_workspace();

    let upload = route_with_body(
        "POST",
        "/files/upload",
        Some(serde_json::json!({
            "runner_id": "file-lab",
            "path": "nested/report.json",
            "content_base64": base64::engine::general_purpose::STANDARD.encode(br#"{"ok":true}"#),
        })),
        &JobStore::default(),
    );
    assert_eq!(upload.status_code, 200, "upload body: {}", upload.body);
    assert_eq!(
        std::fs::read_to_string(workspace.join("nested/report.json")).expect("uploaded file"),
        r#"{"ok":true}"#
    );

    let download = route_with_body(
        "POST",
        "/files/download",
        Some(serde_json::json!({
            "runner_id": "file-lab",
            "path": workspace.join("nested/report.json").display().to_string(),
        })),
        &JobStore::default(),
    );
    assert_eq!(
        download.status_code, 200,
        "download body: {}",
        download.body
    );
    let encoded = download.body["body"]["content_base64"]
        .as_str()
        .expect("content_base64");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("decode content");
    assert_eq!(decoded, br#"{"ok":true}"#);
}
