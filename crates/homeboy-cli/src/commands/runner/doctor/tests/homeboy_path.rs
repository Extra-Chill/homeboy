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

/// Each drift case names its diagnosis; only a clean, strictly newer preferred
/// binary may be recommended through `runner set`.
#[test]
fn lab_homeboy_path_drift_diagnoses_each_preferred_binary() {
    for (configured, preferred, expected) in [
        (
            "0.354.1+current",
            "0.284.0+old",
            Some("older than configured"),
        ),
        (
            "0.353.1+current",
            "0.354.1+new",
            Some("newer than configured"),
        ),
        (
            "0.353.1+current",
            "0.354.1+candidate-dirty",
            Some("dirty build"),
        ),
        (
            "0.354.1+current",
            "0.354.1+different-build",
            Some("different build identity"),
        ),
        ("0.354.1+exact", "0.354.1+exact", None),
    ] {
        let check = probes::lab_homeboy_path_drift_check(
            "lab",
            &configured_homeboy(configured),
            &preferred_homeboy(preferred),
            BTreeMap::new(),
        );
        let Some(fragment) = expected else {
            assert!(check.is_none(), "{preferred}");
            continue;
        };
        let check = check.expect("drift is diagnosed");
        assert_eq!(check.status, RunnerDoctorStatus::Warning, "{preferred}");
        assert!(check.message.contains(fragment), "{}", check.message);
        let remediation = check.remediation.expect("remediation");
        if preferred == "0.354.1+new" {
            assert_eq!(
                remediation,
                "Point runner `lab` at the newer preferred binary with `homeboy runner set lab --json '{\"homeboy_path\":\"/runner/preferred/homeboy\"}'`"
            );
        } else {
            assert!(
                remediation.contains("Update or remove stale preferred binary")
                    && !remediation.contains("runner set"),
                "{remediation}"
            );
        }
    }
}

#[test]
fn older_bare_path_homeboy_than_the_daemon_is_a_finding_with_the_refresh_repair() {
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
    assert_eq!(check.status, RunnerDoctorStatus::Warning);
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
    assert!(check.remediation.as_deref().is_some_and(|value| {
        value.contains("homeboy runner refresh-homeboy homeboy-lab --ref v0.229.9")
    }));
    assert_eq!(
        check.remediation_action,
        Some(types::RunnerRepairAction::RefreshHomeboy {
            git_ref: Some("v0.229.9".to_string()),
            allow_downgrade: false,
        })
    );
}

#[test]
fn newer_bare_path_homeboy_than_the_daemon_is_a_finding() {
    let check = probes::homeboy_path_shadow_check(
        "homeboy-lab",
        "lab-server",
        "/runner/slot/homeboy",
        "0.229.9",
        &configured_homeboy_at("/runner/slot/homeboy", "0.229.9"),
        &probes::RemoteHomeboyCandidateProbe {
            path: Some("/runner/bin/homeboy".to_string()),
            version: Some("0.230.0+abc1234".to_string()),
        },
        BTreeMap::new(),
    )
    .expect("newer bare homeboy is reported");

    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("newer than the runner daemon"));
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains("--ref v0.230.0")));
}

fn configured_homeboy_at(path: &str, version: &str) -> HomeboyProbe {
    HomeboyProbe {
        version: version.to_string(),
        path: Some(path.to_string()),
    }
}

#[test]
fn bare_path_homeboy_at_the_daemon_version_needs_no_repair() {
    let configured = configured_homeboy_at("/home/user/.cargo/bin/homeboy", "0.229.9");
    let shadow = |bare_path: &str| {
        probes::homeboy_path_shadow_check(
            "homeboy-lab",
            "lab-server",
            "/home/user/.cargo/bin/homeboy",
            "0.229.9",
            &configured,
            &probes::RemoteHomeboyCandidateProbe {
                path: Some(bare_path.to_string()),
                version: Some("0.229.9".to_string()),
            },
            BTreeMap::new(),
        )
    };

    let different_path = shadow("/home/user/.local/bin/homeboy").expect("path difference noted");
    assert_eq!(different_path.status, RunnerDoctorStatus::Ok);
    assert!(different_path
        .message
        .contains("/home/user/.local/bin/homeboy"));
    assert!(different_path.remediation.is_none());
    assert!(shadow("/home/user/.cargo/bin/homeboy").is_none());
}

#[test]
fn version_skew_checks_each_exact_build_pair() {
    use homeboy_lab_contract::{LabCapabilityHandshake, LabRuntimeAncestry, LabRuntimeIdentity};
    let identity = |build: &str| LabRuntimeIdentity {
        build_identity: format!("homeboy {build}"),
        lab_contract_version: None,
        source_revision: String::new(),
        clean: true,
    };
    for (daemon, command, expected) in [
        (
            "1.0.0+same",
            "1.0.0+same",
            [RunnerDoctorStatus::Ok, RunnerDoctorStatus::Ok],
        ),
        (
            "1.0.0+other",
            "1.0.0+same",
            [RunnerDoctorStatus::Error, RunnerDoctorStatus::Error],
        ),
        (
            "1.0.0+same",
            "1.0.0+other",
            [RunnerDoctorStatus::Ok, RunnerDoctorStatus::Error],
        ),
        (
            "1.0.0+other",
            "1.0.0+other",
            [RunnerDoctorStatus::Error, RunnerDoctorStatus::Ok],
        ),
        (
            "",
            "",
            [RunnerDoctorStatus::Error, RunnerDoctorStatus::Error],
        ),
    ] {
        let handshake = LabCapabilityHandshake {
            controller: identity("1.0.0+same"),
            runner_command: identity(command),
            daemon: identity(daemon),
            required_capabilities: vec![],
            runner_command_capabilities: vec![],
            daemon_capabilities: vec![],
            ancestry: LabRuntimeAncestry::Unknown,
        };
        let checks = checks::homeboy_version_skew_checks("1.0.0", &handshake, "lab");
        assert_eq!(checks.len(), 2);
        for (check, expected) in checks.iter().zip(expected) {
            assert_eq!(check.status, expected, "{daemon} / {command}");
            assert_eq!(
                check.remediation_action.is_some(),
                expected == RunnerDoctorStatus::Error
            );
        }
        assert_eq!(checks[0].details["daemon_build_identity"], daemon);
        assert_eq!(checks[1].details["homeboy_path_build_identity"], command);
    }
}
