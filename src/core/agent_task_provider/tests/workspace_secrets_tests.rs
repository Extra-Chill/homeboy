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
    crate::test_support::with_isolated_home(|_| {
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
        let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
        request.executor.config = json!({ "provider": "example-oauth" });
        request.executor.secret_env = vec![
            "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
        ];
        provider.provider_defaults.insert(
            "example-oauth".to_string(),
            json!({
                "secret_env": request.executor.secret_env,
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": auth_path,
                        "field": "tokens.access_token"
                    },
                    "EXAMPLE_PROVIDER_REFRESH_TOKEN": {
                        "source": "json-file",
                        "path": auth_path,
                        "field": "tokens.refresh_token"
                    }
                }
            }),
        );

        let env = provider_command_env(&request, &provider).expect("provider env resolves");

        assert!(env.contains(&(
            "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
            "provider-owned-access-token".to_string()
        )));
        let rendered = serde_json::to_string(&provider_secret_sources(&provider, Some(&request)))
            .expect("sources json");
        assert!(!rendered.contains("provider-owned-access-token"));
    });
}

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
    crate::test_support::with_isolated_home(|_| {
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
        let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
        request.executor.config = json!({ "provider": "example-oauth" });
        request.executor.secret_env = vec![
            "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
        ];
        provider.provider_defaults.insert(
            "example-oauth".to_string(),
            json!({
                "secret_env": [
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN",
                    "EXAMPLE_PROVIDER_REFRESH_TOKEN",
                    "EXAMPLE_PROVIDER_EXPIRES_AT"
                ],
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": auth_path.clone(),
                        "field": "provider.access"
                    },
                    "EXAMPLE_PROVIDER_REFRESH_TOKEN": {
                        "source": "json-file",
                        "path": auth_path.clone(),
                        "field": "provider.refresh"
                    },
                    "EXAMPLE_PROVIDER_EXPIRES_AT": {
                        "source": "json-file",
                        "path": auth_path.clone(),
                        "field": "provider.expires"
                    }
                }
            }),
        );

        let env = provider_command_env(&request, &provider).expect("provider env resolves");

        assert!(env.contains(&(
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
            "provider-refresh-token".to_string()
        )));
        assert!(env.contains(&(
            "EXAMPLE_PROVIDER_EXPIRES_AT".to_string(),
            "12345".to_string()
        )));
    });
}

#[test]
fn provider_default_secret_sources_feed_secret_readiness_status() {
    crate::test_support::with_isolated_home(|_| {
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
        let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
        provider.provider_defaults.insert(
            "example-oauth".to_string(),
            json!({
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": auth_path,
                        "field": "tokens.access_token"
                    }
                }
            }),
        );
        let fallback_sources = provider_secret_sources_for_providers(&[provider]);

        let status = crate::core::agent_task_secrets::secret_env_status_with_fallbacks(
            &["EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string()],
            &fallback_sources,
        );

        assert_eq!(status.len(), 1);
        assert!(status[0].configured);
        assert_eq!(status[0].source, "json-file");
    });
}

#[test]
fn provider_workspace_materialization_declares_requires_git_requirement() {
    let (_request, mut provider) = request("task-a", "node provider-a.js".to_string());
    provider.workspace_materialization = Some(AgentTaskProviderWorkspaceMaterialization {
        cwd: None,
        requires_git: Some(true),
        write_scope: Some("artifacts".to_string()),
        artifact_paths: vec![".homeboy/provider".to_string()],
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

#[test]
fn provider_secret_contracts_are_applied_generically() {
    let (mut request, mut provider) = request("task-a", "node provider-a.js".to_string());
    request.executor.config = json!({ "provider": "example-provider" });
    provider.secret_requirements = vec![
        AgentTaskProviderSecretRequirement {
            name: Some("REQUIRED_TOKEN".to_string()),
            required: Some(true),
            ..AgentTaskProviderSecretRequirement::default()
        },
        AgentTaskProviderSecretRequirement {
            name: Some("OPTIONAL_TOKEN".to_string()),
            required: Some(false),
            ..AgentTaskProviderSecretRequirement::default()
        },
    ];
    provider.secret_env_requirements = vec![AgentTaskProviderSecretEnvRequirement {
        env: vec!["EXAMPLE_PROVIDER_TOKEN".to_string()],
        when: Some(json!({
            "any": [
                { "path": "executor.config.provider", "equals": "example-provider" },
                { "path": "provider", "equals": "example-provider" }
            ]
        })),
        ..AgentTaskProviderSecretEnvRequirement::default()
    }];
    provider.provider_defaults.insert(
        "example-provider".to_string(),
        json!({ "secret_env": ["EXAMPLE_PROVIDER_REFRESH_TOKEN"] }),
    );
    let mut plan = AgentTaskPlan::new("plan-a", vec![request]);

    apply_provider_runner_secret_env_contracts_with_providers(&mut plan, &[provider]);

    assert_eq!(
        plan.tasks[0].executor.secret_env,
        vec![
            "EXAMPLE_PROVIDER_REFRESH_TOKEN".to_string(),
            "EXAMPLE_PROVIDER_TOKEN".to_string(),
            "REQUIRED_TOKEN".to_string(),
        ]
    );
}
