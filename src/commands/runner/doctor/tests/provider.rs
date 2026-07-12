use super::super::*;
use homeboy::core::agent_tasks::provider::{
    AgentTaskExecutorProvider, AgentTaskProviderEnvPathReadiness, AgentTaskProviderRunnerReadiness,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use types::RunnerDoctorStatus;

#[test]
fn provider_readiness_renderer_uses_fake_provider_contract() {
    let contract = AgentTaskProviderRunnerReadiness {
        id: "lab.fake_runtime.cache".to_string(),
        label: "Fake runtime cache".to_string(),
        secret_env: Vec::new(),
        env_path: Some(AgentTaskProviderEnvPathReadiness {
            env: vec!["FAKE_RUNTIME_BIN".to_string()],
            revision: Some(true),
            canonical_path: None,
            extra: BTreeMap::new(),
        }),
        executable: None,
        remediation: Some("Refresh the fake runtime cache".to_string()),
        extra: BTreeMap::new(),
    };

    let check = probes::provider_env_path_readiness_check_from_probe(
        &contract,
        Some("/opt/fake-runtime/bin".to_string()),
        true,
        Some("abc123".to_string()),
        None,
    );

    assert_eq!(check.id, "lab.fake_runtime.cache");
    assert_eq!(check.status, RunnerDoctorStatus::Ok);
    assert!(check.message.contains("Fake runtime cache"));
    assert_eq!(
        check.details.get("env").map(String::as_str),
        Some("FAKE_RUNTIME_BIN")
    );
    assert_eq!(
        check.details.get("revision").map(String::as_str),
        Some("abc123")
    );
}

#[test]
fn provider_readiness_warns_on_non_canonical_checkout() {
    let contract = AgentTaskProviderRunnerReadiness {
        id: "lab.fake_runtime.cache".to_string(),
        label: "Fake runtime cache".to_string(),
        secret_env: Vec::new(),
        env_path: Some(AgentTaskProviderEnvPathReadiness {
            env: vec!["FAKE_RUNTIME_BIN".to_string()],
            revision: Some(true),
            canonical_path: Some("/home/runner/.cache/homeboy/source".to_string()),
            extra: BTreeMap::new(),
        }),
        executable: None,
        remediation: Some("Refresh the managed source checkout".to_string()),
        extra: BTreeMap::new(),
    };

    let check = probes::provider_env_path_readiness_check_from_probe(
        &contract,
        Some("/home/runner/Developer/stale-checkout/dist/index.js".to_string()),
        true,
        None,
        Some("/home/runner/.cache/homeboy/source".to_string()),
    );

    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("non-canonical checkout"));
    assert_eq!(
        check.details.get("canonical_path").map(String::as_str),
        Some("/home/runner/.cache/homeboy/source")
    );
    assert_eq!(
        check.remediation.as_deref(),
        contract.remediation.as_deref()
    );
}

#[test]
fn provider_readiness_ok_when_path_within_canonical_root() {
    let contract = AgentTaskProviderRunnerReadiness {
        id: "lab.fake_runtime.cache".to_string(),
        label: "Fake runtime cache".to_string(),
        secret_env: Vec::new(),
        env_path: Some(AgentTaskProviderEnvPathReadiness {
            env: vec!["FAKE_RUNTIME_BIN".to_string()],
            revision: None,
            canonical_path: Some("/home/runner/.cache/homeboy/source".to_string()),
            extra: BTreeMap::new(),
        }),
        executable: None,
        remediation: None,
        extra: BTreeMap::new(),
    };

    let check = probes::provider_env_path_readiness_check_from_probe(
        &contract,
        Some("/home/runner/.cache/homeboy/source/dist/index.js".to_string()),
        true,
        None,
        Some("/home/runner/.cache/homeboy/source".to_string()),
    );

    assert_eq!(check.status, RunnerDoctorStatus::Ok);
}

#[test]
fn path_within_canonical_root_is_segment_aware() {
    assert!(probes::path_within_canonical_root("/a/source", "/a/source"));
    assert!(probes::path_within_canonical_root(
        "/a/source/dist",
        "/a/source"
    ));
    assert!(probes::path_within_canonical_root(
        "/a/source/",
        "/a/source"
    ));
    // Prefix collision must not count as containment.
    assert!(!probes::path_within_canonical_root("/a/sour", "/a/source"));
    assert!(!probes::path_within_canonical_root(
        "/a/source-stale/dist",
        "/a/source"
    ));
    // Empty root is treated as "no canonical constraint".
    assert!(probes::path_within_canonical_root("/anywhere", ""));
}

#[test]
fn local_provider_executor_resolution_check_blocks_broken_require_graph() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("broken-executor.cjs");
    let mut file = std::fs::File::create(&script).expect("create script");
    file.write_all(b"require('./missing-shared-runtime-package');\n")
        .expect("write script");
    let provider = node_provider("test.node.provider", "test", &script);

    let checks = probes::local_provider_executor_resolution_checks(
        &[provider],
        Some("test"),
        Some("test.node.provider"),
    );

    assert_eq!(checks.len(), 1);
    let check = &checks[0];
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(check.id.contains("test.node.provider"));
    assert!(check.message.contains("could not load"));
    assert!(check.details.get("detail").is_some_and(
        |detail| detail.contains("MODULE_NOT_FOUND") || detail.contains("Cannot find module")
    ));
}

#[test]
fn local_provider_executor_resolution_check_filters_to_selected_provider() {
    let dir = tempfile::tempdir().expect("tempdir");
    let selected_script = dir.path().join("selected-executor.cjs");
    std::fs::write(
        &selected_script,
        "if (process.argv.includes('--provider-contract')) process.exit(0); process.exit(2);\n",
    )
    .expect("write selected script");
    let other_script = dir.path().join("other-executor.cjs");
    std::fs::write(&other_script, "require('./missing-package');\n").expect("write other script");
    let selected = node_provider("selected.provider", "test", &selected_script);
    let other = node_provider("other.provider", "other", &other_script);

    let checks = probes::local_provider_executor_resolution_checks(
        &[selected, other],
        Some("test"),
        Some("selected.provider"),
    );

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, RunnerDoctorStatus::Ok);
    assert_eq!(
        checks[0].details.get("provider_id").map(String::as_str),
        Some("selected.provider")
    );
}

#[test]
fn remote_executor_probe_uses_runner_runtime_root_not_controller_path() {
    let provider = node_provider(
        "test.node.provider",
        "test",
        std::path::Path::new(
            "/Users/controller/.config/homeboy/agent-runtimes/test-runtime/scripts/executor.cjs",
        ),
    );
    let mut provider = provider;
    provider.runtime_id = Some("test-runtime".to_string());
    provider.runtime_path =
        Some("/Users/controller/.config/homeboy/agent-runtimes/test-runtime".to_string());
    provider.invocation.cwd = Some("{{runtime_path}}".to_string());

    let entrypoint = probes::remote_provider_executor_entrypoint(&provider)
        .expect("runtime-relative entrypoint");
    let shell = probes::provider_executor_resolution_remote_shell(&entrypoint);

    assert!(matches!(
        entrypoint.args.first(),
        Some(probes::RemoteProviderExecutorEntrypointPart::RuntimeRelative(path)) if path == "scripts/executor.cjs"
    ));
    assert_eq!(entrypoint.cwd.as_deref(), Some(""));
    assert!(shell.contains("$HOME/.config/homeboy/agent-runtimes"));
    assert!(shell.contains("test-runtime"));
    assert!(!shell.contains("/Users/controller"));

    let runner_root = tempfile::tempdir().expect("runner root");
    let script = runner_root.path().join("scripts/executor.cjs");
    std::fs::create_dir_all(script.parent().expect("script parent")).expect("create scripts");
    std::fs::write(&script, "process.exit(0);\n").expect("write runner executor");
    let output = std::process::Command::new("node")
        .arg(runner_root.path().join("scripts/executor.cjs"))
        .arg("--provider-contract")
        .output()
        .expect("run runner-local probe");

    assert!(output.status.success());
}

#[test]
fn remote_executor_probe_keeps_missing_runner_local_dependency_actionable() {
    let provider = node_provider(
        "test.node.provider",
        "test",
        std::path::Path::new(
            "/Users/controller/.config/homeboy/agent-runtimes/test-runtime/scripts/executor.cjs",
        ),
    );
    let mut provider = provider;
    provider.runtime_id = Some("test-runtime".to_string());
    provider.runtime_path =
        Some("/Users/controller/.config/homeboy/agent-runtimes/test-runtime".to_string());

    let entrypoint = probes::remote_provider_executor_entrypoint(&provider)
        .expect("runtime-relative entrypoint");
    assert!(entrypoint
        .display()
        .contains("<runtime:test-runtime>/scripts/executor.cjs"));
    let runner_root = tempfile::tempdir().expect("runner root");
    let script = runner_root.path().join("scripts/executor.cjs");
    std::fs::create_dir_all(script.parent().expect("script parent")).expect("create scripts");
    std::fs::write(&script, "require('./missing-runner-local-package');\n")
        .expect("write runner executor");
    let output = std::process::Command::new("node")
        .arg(runner_root.path().join("scripts/executor.cjs"))
        .arg("--provider-contract")
        .output()
        .expect("run runner-local probe");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot find module"));
}

fn node_provider(id: &str, backend: &str, script: &std::path::Path) -> AgentTaskExecutorProvider {
    serde_json::from_value(json!({
        "id": id,
        "backend": backend,
        "invocation": {
            "argv": ["node", script.display().to_string()]
        }
    }))
    .expect("provider parses")
}
