use super::super::*;
use std::collections::BTreeMap;
use types::{HomeboyProbe, RunnerDoctorStatus};

fn configured_homeboy(version: &str) -> HomeboyProbe {
    HomeboyProbe {
        version: version.to_string(),
        path: Some("/runner/configured/homeboy".to_string()),
    }
}

fn preferred_homeboy(version: &str) -> probes::RemoteHomeboyCandidate {
    probes::RemoteHomeboyCandidate {
        path: "/runner/preferred/homeboy".to_string(),
        version: version.to_string(),
    }
}

#[test]
fn lab_homeboy_path_drift_keeps_a_newer_configured_binary() {
    let check = probes::lab_homeboy_path_drift_check(
        "lab",
        &configured_homeboy("0.354.1+current"),
        &preferred_homeboy("0.284.0+old"),
        BTreeMap::new(),
    )
    .expect("stale preferred binary is diagnosed");

    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("older than configured"));
    assert!(check.message.contains("remains selected"));
    assert!(check.remediation.as_deref().is_some_and(|value| {
        value.contains("Update or remove stale preferred binary") && !value.contains("runner set")
    }));
}

#[test]
fn lab_homeboy_path_drift_recommends_a_clean_newer_preferred_binary() {
    let check = probes::lab_homeboy_path_drift_check(
        "lab",
        &configured_homeboy("0.353.1+current"),
        &preferred_homeboy("0.354.1+new"),
        BTreeMap::new(),
    )
    .expect("newer preferred binary is diagnosed");

    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("newer than configured"));
    assert_eq!(
        check.remediation.as_deref(),
        Some("Point runner `lab` at the newer preferred binary with `homeboy runner set lab --json '{\"homeboy_path\":\"/runner/preferred/homeboy\"}'`")
    );
}

#[test]
fn lab_homeboy_path_drift_rejects_dirty_preferred_binary() {
    let check = probes::lab_homeboy_path_drift_check(
        "lab",
        &configured_homeboy("0.353.1+current"),
        &preferred_homeboy("0.354.1+candidate-dirty"),
        BTreeMap::new(),
    )
    .expect("dirty preferred binary is diagnosed");

    assert!(check.message.contains("dirty build"));
    assert!(check.remediation.as_deref().is_some_and(|value| {
        value.contains("Update or remove stale preferred binary") && !value.contains("runner set")
    }));
}

#[test]
fn lab_homeboy_path_drift_is_absent_for_exact_match() {
    assert!(probes::lab_homeboy_path_drift_check(
        "lab",
        &configured_homeboy("0.354.1+exact"),
        &preferred_homeboy("0.354.1+exact"),
        BTreeMap::new(),
    )
    .is_none());
}

#[test]
fn lab_homeboy_path_drift_keeps_configured_binary_for_different_build_identity() {
    let check = probes::lab_homeboy_path_drift_check(
        "lab",
        &configured_homeboy("0.354.1+current"),
        &preferred_homeboy("0.354.1+different-build"),
        BTreeMap::new(),
    )
    .expect("different build identity is diagnosed");

    assert!(check.message.contains("different build identity"));
    assert!(check.remediation.as_deref().is_some_and(|value| {
        value.contains("Update or remove stale preferred binary") && !value.contains("runner set")
    }));
}

#[test]
fn lab_homeboy_path_shadow_is_ok_when_configured_homeboy_is_current() {
    let mut details = BTreeMap::new();
    details.insert(
        "configured_command".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert(
        "configured_path".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert("configured_version".to_string(), "0.229.9".to_string());
    details.insert(
        "bare_path".to_string(),
        "/home/user/.local/bin/homeboy".to_string(),
    );
    details.insert("bare_version".to_string(), "0.228.22".to_string());

    let check = probes::homeboy_path_shadow_check(
        "homeboy-lab",
        "lab-server",
        "/home/user/.cargo/bin/homeboy",
        "0.229.9",
        &HomeboyProbe {
            version: "0.229.9".to_string(),
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
        },
        &probes::RemoteHomeboyCandidateProbe {
            path: Some("/home/user/.local/bin/homeboy".to_string()),
            version: Some("0.228.22".to_string()),
        },
        details,
    )
    .expect("stale bare homeboy warning");

    assert_eq!(check.id, "lab.homeboy.path_shadow");
    assert_eq!(check.status, RunnerDoctorStatus::Ok);
    assert!(check.message.contains("0.229.9"));
    assert!(check.message.contains("0.228.22"));
    assert_eq!(
        check.details.get("configured_path").map(String::as_str),
        Some("/home/user/.cargo/bin/homeboy")
    );
    assert_eq!(
        check.details.get("bare_path").map(String::as_str),
        Some("/home/user/.local/bin/homeboy")
    );
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("No runner repair is needed")));
}

#[test]
fn lab_homeboy_path_shadow_is_ok_when_configured_and_bare_paths_differ_but_version_matches() {
    let mut details = BTreeMap::new();
    details.insert(
        "configured_command".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert(
        "configured_path".to_string(),
        "/home/user/.cargo/bin/homeboy".to_string(),
    );
    details.insert("configured_version".to_string(), "0.229.9".to_string());
    details.insert(
        "bare_path".to_string(),
        "/home/user/.local/bin/homeboy".to_string(),
    );
    details.insert("bare_version".to_string(), "0.229.9".to_string());

    let check = probes::homeboy_path_shadow_check(
        "homeboy-lab",
        "lab-server",
        "/home/user/.cargo/bin/homeboy",
        "0.229.9",
        &HomeboyProbe {
            version: "0.229.9".to_string(),
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
        },
        &probes::RemoteHomeboyCandidateProbe {
            path: Some("/home/user/.local/bin/homeboy".to_string()),
            version: Some("0.229.9".to_string()),
        },
        details,
    )
    .expect("different bare homeboy path warning");

    assert_eq!(check.id, "lab.homeboy.path_shadow");
    assert_eq!(check.status, RunnerDoctorStatus::Ok);
    assert!(check.message.contains("/home/user/.cargo/bin/homeboy"));
    assert!(check.message.contains("/home/user/.local/bin/homeboy"));
    assert_eq!(
        check.details.get("configured_path").map(String::as_str),
        Some("/home/user/.cargo/bin/homeboy")
    );
    assert_eq!(
        check.details.get("bare_path").map(String::as_str),
        Some("/home/user/.local/bin/homeboy")
    );
    assert!(check.remediation.is_none());
}

#[test]
fn lab_homeboy_path_shadow_accepts_matching_bare_homeboy() {
    let check = probes::homeboy_path_shadow_check(
        "homeboy-lab",
        "lab-server",
        "/home/user/.cargo/bin/homeboy",
        "0.229.9",
        &HomeboyProbe {
            version: "0.229.9".to_string(),
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
        },
        &probes::RemoteHomeboyCandidateProbe {
            path: Some("/home/user/.cargo/bin/homeboy".to_string()),
            version: Some("0.229.9".to_string()),
        },
        BTreeMap::new(),
    );

    assert!(check.is_none());
}

#[test]
fn homeboy_version_skew_check_uses_lab_admission() {
    use super::super::types::RunnerRepairAction;
    use homeboy_lab_contract::{
        required_lab_handoff_capabilities, LabCapabilityHandshake, LabRuntimeAncestry,
        LabRuntimeIdentity, LAB_CONTRACT_VERSION,
    };

    for (
        name,
        remote_build,
        contract_version,
        capabilities_match,
        require_exact,
        expected,
        repair,
    ) in [
        (
            "compatible builds",
            "0.289.0+runner",
            LAB_CONTRACT_VERSION,
            true,
            false,
            RunnerDoctorStatus::Ok,
            false,
        ),
        (
            "contract mismatch",
            "0.289.0+runner",
            LAB_CONTRACT_VERSION + 1,
            true,
            false,
            RunnerDoctorStatus::Error,
            true,
        ),
        (
            "capability mismatch",
            "0.289.0+runner",
            LAB_CONTRACT_VERSION,
            false,
            false,
            RunnerDoctorStatus::Error,
            true,
        ),
        (
            "exact build required",
            "0.289.0+runner",
            LAB_CONTRACT_VERSION,
            true,
            true,
            RunnerDoctorStatus::Error,
            true,
        ),
        (
            "runner ahead",
            "0.291.0+runner",
            LAB_CONTRACT_VERSION,
            true,
            false,
            RunnerDoctorStatus::Error,
            false,
        ),
        (
            "matching builds",
            "0.290.0+controller",
            LAB_CONTRACT_VERSION,
            true,
            false,
            RunnerDoctorStatus::Ok,
            false,
        ),
    ] {
        let controller = LabRuntimeIdentity {
            build_identity: "homeboy 0.290.0+controller".to_string(),
            lab_contract_version: Some(LAB_CONTRACT_VERSION),
            source_revision: String::new(),
            clean: true,
        };
        let remote = LabRuntimeIdentity {
            build_identity: format!("homeboy {remote_build}"),
            lab_contract_version: Some(contract_version),
            ..controller.clone()
        };
        let capabilities = required_lab_handoff_capabilities();
        let handshake = LabCapabilityHandshake {
            controller,
            required_capabilities: capabilities.clone(),
            runner_command: remote.clone(),
            runner_command_capabilities: if capabilities_match {
                capabilities.clone()
            } else {
                Vec::new()
            },
            daemon: remote,
            daemon_capabilities: capabilities,
            ancestry: LabRuntimeAncestry::Unknown,
        };
        let check =
            checks::homeboy_version_skew_check("0.290.0", &handshake, require_exact, "lab", "lab")
                .expect(name);
        assert_eq!(check.status, expected, "{name}");
        assert_eq!(
            check.details["local_build_identity"], "0.290.0+controller",
            "{name}"
        );
        assert_eq!(
            check.details["remote_build_identity"], remote_build,
            "{name}"
        );
        assert_eq!(
            check.remediation.is_some(),
            expected == RunnerDoctorStatus::Error,
            "{name}"
        );
        if repair {
            assert!(
                check
                    .remediation
                    .as_deref()
                    .unwrap()
                    .contains("homeboy runner connect"),
                "{name}"
            );
            assert!(
                matches!(check.remediation_action, Some(RunnerRepairAction::RefreshHomeboy { git_ref: Some(ref git_ref), allow_downgrade: false }) if git_ref == "v0.290.0"),
                "{name}"
            );
        } else {
            assert!(check.remediation_action.is_none(), "{name}");
        }
        if name == "runner ahead" {
            assert_eq!(check.details["direction"], "runner_ahead");
            assert!(check
                .remediation
                .unwrap()
                .contains("Upgrade the controller"));
        }
    }
}

/// The prose and the typed action must name the same ref.
///
/// They are two vocabularies of one fix -- a sentence an operator reads and
/// arguments a repair driver runs -- and nothing structural keeps them in step.
/// If the format string and the action are ever edited apart, `--repair` would
/// materialize a different build than the remediation told the operator to,
/// which is worse than either being wrong alone. This is that tie (#13551).
#[test]
fn a_check_without_an_automatic_repair_carries_no_action() {
    let check = checks::warning(
        "example.manual",
        "needs a human decision".to_string(),
        Some("Ask the operator".to_string()),
    );
    assert!(check.remediation.is_some());
    assert!(check.remediation_action.is_none());
}
