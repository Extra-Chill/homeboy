use super::super::*;
use homeboy::core::agent_tasks::provider::AgentTaskProviderRunnerSource;
use std::collections::BTreeMap;
use types::RunnerDoctorStatus;

#[test]
fn managed_runner_source_warns_on_dirty_generated_cache_state() {
    let contract = AgentTaskProviderRunnerSource {
        id: "sample-runtime".to_string(),
        label: "Managed Sandbox".to_string(),
        path: "/home/chubes/.cache/homeboy/sample-runtime/source".to_string(),
        remote_url: Some("https://github.com/Automattic/sample-runtime.git".to_string()),
        git_ref: Some("main".to_string()),
        remediation: Some("Run runner doctor with --repair".to_string()),
        extra: BTreeMap::new(),
    };
    let mut details = BTreeMap::new();
    details.insert("dirty_files".to_string(), "1".to_string());

    let check = probes::managed_runner_source_state_check(
        &contract,
        "lab.managed_source.sample-runtime".to_string(),
        Some("main"),
        1,
        details,
    )
    .expect("dirty source warning");

    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check
        .message
        .contains("reconstructable local modifications"));
    assert_eq!(
        check.details.get("dirty_files").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        check.remediation.as_deref(),
        contract.remediation.as_deref()
    );
}

#[test]
fn managed_runner_source_warns_on_detached_declared_ref() {
    let contract = AgentTaskProviderRunnerSource {
        id: "sample-runtime".to_string(),
        label: "Managed Sandbox".to_string(),
        path: "/home/chubes/.cache/homeboy/sample-runtime/source".to_string(),
        remote_url: Some("https://github.com/Automattic/sample-runtime.git".to_string()),
        git_ref: Some("main".to_string()),
        remediation: Some("Run runner doctor with --repair".to_string()),
        extra: BTreeMap::new(),
    };

    let check = probes::managed_runner_source_state_check(
        &contract,
        "lab.managed_source.sample-runtime".to_string(),
        None,
        0,
        BTreeMap::new(),
    )
    .expect("detached source warning");

    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("declared ref `main`"));
    assert_eq!(
        check.remediation.as_deref(),
        contract.remediation.as_deref()
    );
}
