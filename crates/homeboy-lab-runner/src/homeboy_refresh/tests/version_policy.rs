use super::*;

#[test]
fn release_refs_exclude_unreleased_targets() {
    for reference in ["main", "abcdef123", "1.2.3", "v1.2.3-dev", "v1.2.3+build"] {
        assert!(release_version(reference).is_err(), "{reference}");
    }
    assert_eq!(release_version("v1.2.3").unwrap().to_string(), "1.2.3");
}

#[test]
fn downgrade_policy_compares_versions_and_records_authorized_rollbacks() {
    let plan = ssh_bootstrap_plan();
    let authorities = RefreshPromotionAuthorities {
        controller: Some("1.2.3".into()),
        active_daemon: Some("1.3.0".into()),
        configured_selected: Some("1.2.0".into()),
    };
    for version in ["1.3.0", "1.10.0"] {
        let candidate = serde_json::json!({"version": version});
        assert!(
            validate_refresh_promotion(&plan, &candidate, false, &authorities)
                .unwrap()
                .is_none()
        );
    }
    let candidate = serde_json::json!({"version": "1.2.3"});
    assert!(validate_refresh_promotion(&plan, &candidate, false, &authorities).is_err());
    let rollback = validate_refresh_promotion(&plan, &candidate, true, &authorities)
        .unwrap()
        .unwrap();
    assert_eq!(rollback.previous, ["active daemon:1.3.0"]);
    assert_eq!(rollback.selected, "1.2.3");
    assert!(validate_refresh_promotion(&plan, &serde_json::json!({}), true, &authorities).is_err());
}

#[test]
fn release_identity_must_match_requested_version() {
    let plan = ssh_bootstrap_plan();
    for (version, dirty, accepted) in [
        ("1.2.3", false, true),
        ("1.2.4", false, false),
        ("1.2.3", true, false),
    ] {
        let identity =
            serde_json::json!({"version": version, "git_commit": "abc", "git_dirty": dirty});
        assert_eq!(
            verify_materialized_identity(&plan, "", &identity).is_ok(),
            accepted
        );
    }
}
