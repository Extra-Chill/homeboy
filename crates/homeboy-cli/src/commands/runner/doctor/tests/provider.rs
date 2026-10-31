use super::super::*;
use homeboy::agents::agent_tasks::provider::{
    AgentTaskProviderEnvPathReadiness, AgentTaskProviderRunnerReadiness,
    AgentTaskProviderRunnerReadinessContract,
};
use homeboy::core::command_invocation::CommandInvocation;
use std::collections::BTreeMap;
use std::process::Command;
use types::RunnerDoctorStatus;

fn readiness_contract() -> AgentTaskProviderRunnerReadiness {
    AgentTaskProviderRunnerReadiness {
        id: "lab.fake_runtime.cache".to_string(),
        label: "Fake runtime cache".to_string(),
        required_extensions: Vec::new(),
        invocation: None,
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
    }
}

#[test]
fn provider_readiness_renderer_uses_fake_provider_contract() {
    let check = probes::provider_env_path_readiness_check_from_probe(
        &readiness_contract(),
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
    let mut contract = readiness_contract();
    contract.env_path.as_mut().unwrap().canonical_path =
        Some("/home/runner/.cache/homeboy/source".to_string());
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
    let mut contract = readiness_contract();
    contract.env_path.as_mut().unwrap().canonical_path =
        Some("/home/runner/.cache/homeboy/source".to_string());
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
    assert!(!probes::path_within_canonical_root("/a/sour", "/a/source"));
    assert!(!probes::path_within_canonical_root(
        "/a/source-stale/dist",
        "/a/source"
    ));
    assert!(probes::path_within_canonical_root("/anywhere", ""));
}

#[test]
fn selected_provider_readiness_contracts_do_not_probe_unselected_providers() {
    let contracts = ["selected.provider", "unselected.provider"]
        .into_iter()
        .map(|provider_id| AgentTaskProviderRunnerReadinessContract {
            provider_id: provider_id.to_string(),
            backend: "test".to_string(),
            runtime_id: Some("test-runtime".to_string()),
            runtime_path: Some("/runtime".to_string()),
            readiness: readiness_contract(),
        })
        .collect();
    let selected = probes::selected_provider_readiness_contracts(
        contracts,
        &["selected.provider".to_string()],
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].provider_id, "selected.provider");
}

#[test]
fn command_readiness_failure_preserves_bounded_child_output() {
    let check = probes::provider_command_readiness_failure_check(
        "lab.fixture.readiness",
        "Managed runner readiness",
        Some("homeboy extension refresh <source> --id fixture-extension".to_string()),
        BTreeMap::from([("provider_id".to_string(), "fixture.provider".to_string())]),
        false,
        "Cannot find module './missing-extension-module'\nRequire stack:\n- /runner/runtime/readiness.cjs",
        "",
    );
    assert_eq!(check.id, "lab.fixture.readiness");
    assert_eq!(check.status, RunnerDoctorStatus::Error);
    assert!(check
        .message
        .contains("Cannot find module './missing-extension-module'"));
    assert_eq!(check.details.get("stdout").map(String::as_str), Some("Cannot find module './missing-extension-module'\nRequire stack:\n- /runner/runtime/readiness.cjs"));
    assert_eq!(
        check.remediation.as_deref(),
        Some("homeboy extension refresh <source> --id fixture-extension")
    );
}

#[test]
fn command_readiness_surfaces_incomplete_cache_repair() {
    let controller_runtime = "/Users/controller/.config/homeboy/agent-runtimes/cache-fixture";
    let invocation = CommandInvocation {
        argv: vec![
            "node".to_string(),
            "{{runtime_path}}/scripts/readiness.cjs".to_string(),
        ],
        ..CommandInvocation::default()
    };
    let entrypoint =
        probes::remote_readiness_entrypoint("cache-fixture", controller_runtime, &invocation)
            .expect("runtime-relative readiness invocation");
    let home = tempfile::tempdir().expect("runner home");
    let script = home
        .path()
        .join(".config/homeboy/agent-runtimes/cache-fixture/scripts/readiness.cjs");
    std::fs::create_dir_all(script.parent().expect("script parent")).expect("create scripts");
    std::fs::write(&script, "process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:false,classification:'incomplete_cache',retryable:false,reason:'built CLI is missing',remediation:'repair the managed runtime cache',identity:{runtime:'cache-fixture'}}));")
        .expect("write readiness script");
    let shell = probes::provider_readiness_remote_shell(
        &entrypoint,
        r#"{"schema":"homeboy/agent-task-provider-readiness-request/v1"}"#,
    );
    assert!(!shell.contains(controller_runtime));
    let output = Command::new("sh")
        .arg("-c")
        .arg(shell)
        .env("HOME", home.path())
        .output()
        .expect("run runner-local readiness");
    assert!(output.status.success());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("structured readiness result");
    assert_eq!(result["ready"], false);
    assert_eq!(result["classification"], "incomplete_cache");
    assert_eq!(result["reason"], "built CLI is missing");
    assert_eq!(result["remediation"], "repair the managed runtime cache");
    assert_eq!(result["identity"]["runtime"], "cache-fixture");
}
