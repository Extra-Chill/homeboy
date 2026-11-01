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
