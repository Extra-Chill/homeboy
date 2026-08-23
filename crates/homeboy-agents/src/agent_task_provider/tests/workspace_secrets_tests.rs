use super::common::request;
use super::*;

#[test]
fn provider_secret_sources_for_providers_include_default_json_sources() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.provider_defaults.insert(
        "example-oauth".to_string(),
        json!({
            "secret_env": ["EXAMPLE_PROVIDER_ACCESS_TOKEN"],
            "secret_env_sources": {
                "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                    "source": "json-file",
                    "path": "~/.example-provider/auth.json",
                    "field": "tokens.access_token"
                }
            }
        }),
    );

    let sources = provider_secret_sources_for_providers(&[provider]);

    let source = sources
        .get("EXAMPLE_PROVIDER_ACCESS_TOKEN")
        .expect("provider default source discovered");
    assert_eq!(source.source, "json-file");
    assert_eq!(
        source.path.as_deref(),
        Some("~/.example-provider/auth.json")
    );
    assert_eq!(source.field.as_deref(), Some("tokens.access_token"));
}

#[test]
fn provider_default_secret_sources_accept_nested_json_sources() {
    let temp = tempfile::tempdir().expect("tempdir");
    let auth_path = temp.path().join("provider-auth.json");
    fs::write(
        &auth_path,
        json!({
            "provider": {
                "access": "provider-access-token",
                "refresh": "provider-refresh-token",
                "expires": 12345
            }
        })
        .to_string(),
    )
    .expect("write auth");
    let auth_path = auth_path.to_string_lossy().to_string();
    let access = format!("HOMEBOY_TEST_ACCESS_{}", uuid::Uuid::new_v4());
    let refresh = format!("HOMEBOY_TEST_REFRESH_{}", uuid::Uuid::new_v4());
    let expires = format!("HOMEBOY_TEST_EXPIRES_{}", uuid::Uuid::new_v4());
    let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
    request.executor.config = json!({ "provider": "example-oauth" });
    request.executor.secret_env = vec![access.clone(), refresh.clone(), expires.clone()];
    provider.provider_defaults.insert(
        "example-oauth".to_string(),
        json!({
            "secret_env": request.executor.secret_env,
            "secret_env_sources": {
                access: {
                    "source": "json-file",
                    "path": auth_path.clone(),
                    "field": "provider.access"
                },
                refresh.clone(): {
                    "source": "json-file",
                    "path": auth_path.clone(),
                    "field": "provider.refresh"
                },
                expires.clone(): {
                    "source": "json-file",
                    "path": auth_path.clone(),
                    "field": "provider.expires"
                }
            }
        }),
    );

    let env = provider_command_env(&request, &provider).expect("provider env resolves");

    assert!(env.contains(&(refresh, "provider-refresh-token".to_string())));
    assert!(env.contains(&(expires, "12345".to_string())));
}

#[test]
fn provider_default_secret_sources_feed_secret_readiness_status() {
    let temp = tempfile::tempdir().expect("tempdir");
    let auth_path = temp.path().join("provider-auth.json");
    fs::write(
        &auth_path,
        json!({
            "tokens": {
                "access_token": "provider-owned-access-token"
            }
        })
        .to_string(),
    )
    .expect("write auth");
    let access = format!("HOMEBOY_TEST_ACCESS_{}", uuid::Uuid::new_v4());
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.provider_defaults.insert(
        "example-oauth".to_string(),
        json!({
            "secret_env_sources": {
                access.clone(): {
                    "source": "json-file",
                    "path": auth_path,
                    "field": "tokens.access_token"
                }
            }
        }),
    );
    let fallback_sources = provider_secret_sources_for_providers(&[provider]);

    let status =
        crate::agent_task_secrets::secret_env_status_with_fallbacks(&[access], &fallback_sources);

    assert_eq!(status.len(), 1);
    assert!(status[0].configured);
    assert_eq!(status[0].source, "json-file");
}

#[test]
fn provider_workspace_materialization_exports_typed_mount_specs() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: Some("workspace".to_string()),
        mounts: vec![WorkspaceMountSpec {
            handle: Some("homeboy@fix-workspace-materialization-spec".to_string()),
            repo: Some("homeboy".to_string()),
            host_path: Some("/host/workspaces/homeboy@fix".to_string()),
            target_path: Some("/workspace/homeboy".to_string()),
            mode: Some("read_write".to_string()),
            materialization: Some("bind_mount".to_string()),
            metadata: json!({ "source": "fixture" }),
            extra: BTreeMap::new(),
        }],
        ..AgentTaskProviderWorkspaceMaterialization::default()
    });

    let exported = serde_json::to_value(&provider).expect("provider json");

    assert_eq!(
        exported["workspace_materialization"]["mounts"][0]["handle"],
        "homeboy@fix-workspace-materialization-spec"
    );
    assert_eq!(
        exported["workspace_materialization"]["mounts"][0]["target_path"],
        "/workspace/homeboy"
    );
    assert_eq!(
        exported["workspace_materialization"]["mounts"][0]["materialization"],
        "bind_mount"
    );
}
