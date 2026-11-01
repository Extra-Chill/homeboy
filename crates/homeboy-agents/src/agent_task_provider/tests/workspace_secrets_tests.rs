use super::common::request;
use super::*;

#[test]
fn provider_workspace_materialization_declares_cwd_git_checkout_requirement() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: Some(WorkspaceCwdMode::GitCheckout.to_string()),
        requires_git: None,
        write_scope: None,
        artifact_paths: Vec::new(),
        spec: None,
        mounts: Vec::new(),
        apply_back: AgentTaskRuntimeApplyBack::default(),
        extra: BTreeMap::new(),
    });

    assert!(provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "test",
        None
    ));
}

#[test]
fn provider_default_secret_sources_resolve_required_env_without_duplicate_mapping() {
    let temp = tempfile::tempdir().expect("tempdir");
    let auth_path = temp.path().join("provider-auth.json");
    fs::write(
        &auth_path,
        json!({
            "tokens": {
                "access_token": "provider-owned-access-token",
                "refresh_token": "provider-owned-refresh-token"
            }
        })
        .to_string(),
    )
    .expect("write auth");
    let access = format!("HOMEBOY_TEST_ACCESS_{}", uuid::Uuid::new_v4());
    let refresh = format!("HOMEBOY_TEST_REFRESH_{}", uuid::Uuid::new_v4());
    let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
    request.executor.config = json!({ "provider": "example-oauth" });
    request.executor.secret_env = vec![access.clone(), refresh.clone()];
    provider.provider_defaults.insert(
        "example-oauth".to_string(),
        json!({
            "secret_env": request.executor.secret_env,
            "secret_env_sources": {
                access.clone(): {
                    "source": "json-file",
                    "path": auth_path,
                    "field": "tokens.access_token"
                },
                refresh: {
                    "source": "json-file",
                    "path": auth_path,
                    "field": "tokens.refresh_token"
                }
            }
        }),
    );

    let env = provider_command_env(&request, &provider).expect("provider env resolves");

    assert!(env.contains(&(access, "provider-owned-access-token".to_string())));
    let rendered = serde_json::to_string(&provider_secret_sources(&provider, Some(&request)))
        .expect("sources json");
    assert!(!rendered.contains("provider-owned-access-token"));
}

#[test]
fn provider_credential_mappings_reject_conflicting_accounts_for_one_executor() {
    let (mut first, mut provider) = request("task-a", "node provider-a.js".to_string());
    let (mut second, _) = request("task-b", "node provider-a.js".to_string());
    first.executor.config = json!({ "provider": "account-a" });
    second.executor.config = json!({ "provider": "account-b" });
    provider.provider_defaults.insert(
        "account-a".to_string(),
        json!({
            "secret_env": ["PROVIDER_ACCESS_TOKEN"],
            "secret_env_sources": {
                "PROVIDER_ACCESS_TOKEN": {
                    "source": "json-file",
                    "path": "~/.provider/a.json",
                    "field": "tokens.access_token"
                }
            }
        }),
    );
    provider.provider_defaults.insert(
        "account-b".to_string(),
        json!({
            "secret_env": ["PROVIDER_ACCESS_TOKEN"],
            "secret_env_sources": {
                "PROVIDER_ACCESS_TOKEN": {
                    "source": "json-file",
                    "path": "~/.provider/b.json",
                    "field": "tokens.access_token"
                }
            }
        }),
    );

    let error = provider_secret_credential_mappings_for_plan_with_providers(
        &AgentTaskPlan::new("conflicting-accounts", vec![first, second]),
        &[provider],
    )
    .expect_err("one provider id cannot silently overwrite account mappings");

    assert!(error
        .message
        .contains("conflicting credential source mappings"));
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
fn provider_workspace_materialization_declares_requires_git_requirement() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: None,
        requires_git: Some(true),
        write_scope: Some("artifacts".to_string()),
        artifact_paths: vec!["provider-artifacts".to_string()],
        spec: None,
        mounts: Vec::new(),
        apply_back: AgentTaskRuntimeApplyBack::default(),
        extra: BTreeMap::new(),
    });

    assert!(provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "test",
        None
    ));
}

#[test]
fn provider_apply_back_contract_declares_git_checkout_requirement() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        apply_back: AgentTaskRuntimeApplyBack {
            requires_git_checkout: Some(true),
            strategy: Some(AgentTaskApplyBackStrategy::MutationArtifacts.to_string()),
            mutation_artifacts: vec![AgentTaskRuntimeMutationArtifact {
                name: "patch".to_string(),
                path: "outputs.runtime.artifacts.patch".to_string(),
                kind: Some("patch".to_string()),
                semantic_key: Some("workspace.patch".to_string()),
                apply_method: Some("git_apply".to_string()),
            }],
        },
        ..AgentTaskProviderWorkspaceMaterialization::default()
    });

    assert!(provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "test",
        None
    ));
}

#[test]
fn provider_workspace_materialization_ignores_unselected_provider() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: Some(WorkspaceCwdMode::GitCheckout.to_string()),
        requires_git: None,
        write_scope: None,
        artifact_paths: Vec::new(),
        spec: None,
        mounts: Vec::new(),
        apply_back: AgentTaskRuntimeApplyBack::default(),
        extra: BTreeMap::new(),
    });

    assert!(!provider_requires_cwd_git_checkout_with_providers(
        &[provider],
        "other",
        None
    ));
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

#[test]
fn workspace_materialization_spec_validates_nested_mounts() {
    let materialization = AgentTaskProviderWorkspaceMaterialization {
        spec: Some(WorkspaceMaterializationSpec {
            materialization: Some("bind_mount".to_string()),
            mounts: vec![WorkspaceMountSpec {
                host_path: Some("/tmp/homeboy".to_string()),
                target_path: Some(" ".to_string()),
                ..WorkspaceMountSpec::default()
            }],
            ..WorkspaceMaterializationSpec::default()
        }),
        mounts: vec![WorkspaceMountSpec {
            host_path: Some("/tmp/homeboy".to_string()),
            ..WorkspaceMountSpec::default()
        }],
        ..AgentTaskProviderWorkspaceMaterialization::default()
    };

    let errors = materialization.validate().expect_err("validation errors");

    assert_eq!(
        errors,
        vec![
            "spec.mounts[0].target_path must not be blank".to_string(),
            "mounts[0].target_path is required when host_path is set".to_string(),
        ]
    );
}
