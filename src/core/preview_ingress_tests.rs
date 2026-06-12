use super::preview_ingress::*;
use crate::test_support;

#[test]
fn route_registers_host_to_upstream_origin() {
    test_support::with_isolated_home(|_| {
        let route = register_route(PreviewIngressRoute {
            session_id: "run-123".to_string(),
            public_host: "run-123-tunnel.chubes.net".to_string(),
            upstream_origin: "http://127.0.0.1:7331".to_string(),
            expires_at: None,
            active: true,
        })
        .expect("register route");

        assert_eq!(route.session_id, "run-123");
        let status = status(
            Some("127.0.0.1:7350".to_string()),
            Some("chubes.net".to_string()),
            Some("*-tunnel.chubes.net".to_string()),
        )
        .expect("status");
        assert_eq!(status.routes.len(), 1);
        assert_eq!(
            status.routes[0].lifecycle,
            PreviewIngressRouteLifecycle::Active
        );
        assert_eq!(
            status.routes[0].route.public_host,
            "run-123-tunnel.chubes.net"
        );
    });
}

#[test]
fn route_status_reports_expired_and_disconnected_sessions() {
    test_support::with_isolated_home(|_| {
        register_route(PreviewIngressRoute {
            session_id: "expired".to_string(),
            public_host: "expired-tunnel.chubes.net".to_string(),
            upstream_origin: "http://127.0.0.1:7331".to_string(),
            expires_at: Some("2000-01-01T00:00:00Z".to_string()),
            active: true,
        })
        .expect("register expired route");
        register_route(PreviewIngressRoute {
            session_id: "disconnected".to_string(),
            public_host: "disconnected-tunnel.chubes.net".to_string(),
            upstream_origin: "http://127.0.0.1:7332".to_string(),
            expires_at: None,
            active: false,
        })
        .expect("register disconnected route");

        let routes = status(None, None, None).expect("status").routes;
        assert_eq!(routes.len(), 2);
        assert_eq!(
            routes[0].lifecycle,
            PreviewIngressRouteLifecycle::Disconnected
        );
        assert_eq!(routes[1].lifecycle, PreviewIngressRouteLifecycle::Expired);
    });
}

#[test]
fn route_validation_rejects_non_http_upstream_origin() {
    test_support::with_isolated_home(|_| {
        let err = register_route(PreviewIngressRoute {
            session_id: "bad".to_string(),
            public_host: "bad-tunnel.chubes.net".to_string(),
            upstream_origin: "ssh://127.0.0.1:22".to_string(),
            expires_at: None,
            active: true,
        })
        .expect_err("non-http upstream should fail");

        assert!(err.message.contains("upstream origin"));
    });
}

fn install_options() -> PreviewIngressInstallOptions {
    PreviewIngressInstallOptions {
        server_id: "preview-vps".to_string(),
        domain: "example.com".to_string(),
        public_host_pattern: "*-tunnel.example.com".to_string(),
        ..PreviewIngressInstallOptions::default()
    }
}

#[test]
fn install_plan_renders_generic_non_secret_operator_config() {
    let plan = render_install_plan(install_options()).expect("plan");

    assert_eq!(plan.server_id, "preview-vps");
    assert_eq!(plan.dns_probe_host, "homeboy-health-tunnel.example.com");
    assert!(plan.systemd_unit.contains("Homeboy preview ingress"));
    assert!(plan.systemd_unit.contains("tunnel preview-ingress serve"));
    assert!(plan.systemd_unit.contains("--public-host-pattern"));
    assert!(plan.nginx_site.contains("server_name *-tunnel.example.com"));
    assert!(plan
        .caddy_site
        .contains("reverse_proxy http://127.0.0.1:7350"));
    assert!(plan
        .secrets_policy
        .iter()
        .any(|item| item.contains("non-secret")));
    assert!(plan
        .required_operator_config
        .iter()
        .any(|item| item.contains("Wildcard DNS")));
    assert!(plan.dry_run);
    assert!(!plan.applied);
}

#[test]
fn install_status_plan_is_machine_readable_without_live_probe() {
    let status = render_install_status_plan(install_options()).expect("status");

    assert!(!status.probed);
    assert_eq!(status.checks.len(), 5);
    assert!(status
        .checks
        .iter()
        .all(|check| check.status == PreviewIngressInstallCheckStatus::Planned));
}

#[test]
fn install_validation_rejects_public_bind_and_non_wildcard_pattern() {
    let public_bind = render_install_plan(PreviewIngressInstallOptions {
        bind: "0.0.0.0:7350".to_string(),
        ..install_options()
    })
    .expect_err("public bind rejected");
    assert!(public_bind.message.contains("loopback"));

    let fixed_host = render_install_plan(PreviewIngressInstallOptions {
        public_host_pattern: "preview.example.com".to_string(),
        ..install_options()
    })
    .expect_err("non-wildcard rejected");
    assert!(fixed_host.message.contains("wildcard"));
}
