use super::super::*;
use homeboy::core::agent_tasks::provider::{
    AgentTaskProviderEnvPathReadiness, AgentTaskProviderRunnerReadiness,
};
use std::collections::BTreeMap;
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
