use super::super::*;
use std::collections::BTreeMap;
use types::{HomeboyProbe, RunnerDoctorStatus};

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
fn homeboy_version_skew_check_is_absent_for_matching_build_identities() {
    assert!(checks::homeboy_version_skew_check(
        "0.290.0",
        "homeboy 0.290.0+00d2756ef115",
        "0.290.0+00d2756ef115",
        "lab",
        "lab",
    )
    .is_none());
}

#[test]
fn homeboy_version_skew_check_warns_for_different_build_identities() {
    let check = checks::homeboy_version_skew_check(
        "0.290.0",
        "homeboy 0.290.0+00d2756ef115",
        "0.290.0+differentbuild",
        "lab",
        "lab",
    )
    .expect("version skew warning");

    assert_eq!(check.id, "homeboy.version_skew");
    assert_eq!(check.status, RunnerDoctorStatus::Warning);
    assert!(check.message.contains("0.290.0+00d2756ef115"));
    assert!(check.message.contains("0.290.0+differentbuild"));
    assert_eq!(
        check.details.get("local_version").map(String::as_str),
        Some("0.290.0")
    );
    assert_eq!(
        check
            .details
            .get("local_build_identity")
            .map(String::as_str),
        Some("0.290.0+00d2756ef115")
    );
    assert_eq!(
        check.details.get("remote_version").map(String::as_str),
        Some("0.290.0+differentbuild")
    );
    let expected_ref = homeboy_product_identity::build_identity()
        .git_commit
        .unwrap_or_else(|| "v0.290.0".to_string());
    assert!(check
        .remediation
        .as_deref()
        .is_some_and(|value| value.contains(&format!(
            "homeboy runner refresh-homeboy lab --ref {expected_ref} --reconnect"
        ))));
}

#[test]
fn homeboy_version_skew_check_warns_for_different_semantic_versions() {
    assert!(checks::homeboy_version_skew_check(
        "0.290.0",
        "homeboy 0.290.0+00d2756ef115",
        "0.289.0+00d2756ef115",
        "lab",
        "lab",
    )
    .is_some());
}
