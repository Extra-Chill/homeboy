use super::*;
use crate::notification_route::NotificationRoute;

fn comparison(verdict: Verdict, candidate: &str) -> RegressionComparison {
    RegressionComparison {
        baseline_version: Some("0.1.0".into()),
        candidate_version: Some(candidate.into()),
        verdict,
        regressions: vec![],
    }
}
fn install_transport(command: &str) {
    let mut manifest: homeboy_extension_contract::ExtensionManifest = serde_json::from_value(serde_json::json!({
        "name": "Regression test transport",
        "version": "1.0.0",
        "notification_transports": [{
            "schema": homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA,
            "id": "regression.test", "command": [command]
        }]
    })).unwrap();
    manifest.id = "regression-test".into();
    crate::extension::catalog::save_manifest(&manifest).unwrap();
}
#[test]
fn clean_and_insufficient_do_not_notify_or_claim() {
    crate::test_support::with_isolated_home(|_| {
        install_transport("true");
        let route = NotificationRoute::new("regression.test", "destination").unwrap();
        for verdict in [Verdict::Clean, Verdict::InsufficientSamples] {
            notify_regression(&comparison(verdict, "0.2.0"), Some(&route));
        }
        assert!(!crate::paths::homeboy()
            .unwrap()
            .join("hotspot-notifications")
            .exists());
        assert!(crate::notify_outbox::pending_entries().is_empty());
    });
}
#[test]
fn repeated_regression_claims_once_per_pair() {
    crate::test_support::with_isolated_home(|_| {
        install_transport("true");
        let route = NotificationRoute::new("regression.test", "destination").unwrap();
        let first = comparison(Verdict::Regressed, "0.2.0");
        notify_regression(&first, Some(&route));
        let claims = crate::paths::homeboy()
            .unwrap()
            .join("hotspot-notifications");
        let count = || std::fs::read_dir(&claims).unwrap().count();
        assert_eq!(count(), 1);
        notify_regression(&first, Some(&route));
        assert_eq!(count(), 1);
        notify_regression(&comparison(Verdict::Regressed, "0.3.0"), Some(&route));
        assert_eq!(count(), 2);
    });
}
#[test]
fn queued_regression_is_not_enqueued_again() {
    crate::test_support::with_isolated_home(|_| {
        install_transport("false");
        let route = NotificationRoute::new("regression.test", "destination").unwrap();
        let report = comparison(Verdict::Regressed, "0.2.0");
        notify_regression(&report, Some(&route));
        assert_eq!(crate::notify_outbox::pending_entries().len(), 1);
        notify_regression(&report, Some(&route));
        assert_eq!(crate::notify_outbox::pending_entries().len(), 1);
    });
}
#[test]
fn dropped_delivery_releases_pair_for_retry() {
    crate::test_support::with_isolated_home(|_| {
        let route = NotificationRoute::new("regression.test", "destination").unwrap();
        notify_regression(&comparison(Verdict::Regressed, "0.2.0"), Some(&route));
        let claims = crate::paths::homeboy()
            .unwrap()
            .join("hotspot-notifications");
        assert_eq!(std::fs::read_dir(claims).unwrap().count(), 0);
    });
}
