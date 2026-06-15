use super::preview_ingress::*;
use super::tunnel::native_preview_token_sha256;
use crate::test_support;
use base64::Engine;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

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
fn status_for_host_reports_route_registration_state() {
    test_support::with_isolated_home(|_| {
        register_route(PreviewIngressRoute {
            session_id: "run-123".to_string(),
            public_host: "run-123-tunnel.chubes.net".to_string(),
            upstream_origin: "http://127.0.0.1:7331".to_string(),
            expires_at: None,
            active: true,
        })
        .expect("register route");

        let status = status_for_host(
            None,
            None,
            None,
            Some("RUN-123-TUNNEL.CHUBES.NET:443".to_string()),
        )
        .expect("status");

        assert_eq!(
            status.inspected_host.as_deref(),
            Some("run-123-tunnel.chubes.net")
        );
        assert_eq!(status.inspected_state.as_deref(), Some("registered"));
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

#[test]
fn reverse_channel_client_serves_public_request() {
    test_support::with_isolated_home(|_| {
        let token = "test-preview-token";
        std::env::set_var(
            "HOMEBOY_TEST_PREVIEW_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let port = listener.local_addr().expect("local addr").port();
        thread::spawn(move || {
            serve_listener(
                PreviewIngressServeSpec {
                    bind: format!("127.0.0.1:{port}"),
                    domain: "example.com".to_string(),
                    public_host_pattern: "*-tunnel.example.com".to_string(),
                    token_sha256_env: "HOMEBOY_TEST_PREVIEW_TOKEN_SHA256".to_string(),
                },
                listener,
            )
            .expect("serve ingress");
        });
        thread::sleep(Duration::from_millis(100));

        let register = http_request(
            port,
            "POST",
            "/preview/client/register",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({
                "public_host": "run-1-tunnel.example.com",
                "local_origin": "http://127.0.0.1:49999",
                "session_id": "run-1"
            })
            .to_string(),
        );
        assert!(register.contains("200 OK"), "{register}");

        let browser = thread::spawn(move || {
            raw_http_request(
                port,
                "GET /assets/app.js?ver=1 HTTP/1.1\r\nHost: run-1-tunnel.example.com\r\n\r\n",
            )
        });
        thread::sleep(Duration::from_millis(100));

        let next = http_request(
            port,
            "POST",
            "/preview/client/next",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({ "public_host": "run-1-tunnel.example.com", "timeout_secs": 2 }).to_string(),
        );
        assert!(next.contains("/assets/app.js?ver=1"), "{next}");
        let request_id = response_json(&next)["request"]["request_id"]
            .as_str()
            .expect("request id")
            .to_string();

        let respond = http_request(
            port,
            "POST",
            "/preview/client/respond",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({
                "public_host": "run-1-tunnel.example.com",
                "response": {
                    "request_id": request_id,
                    "status": 200,
                    "headers": { "content-type": "application/javascript" },
                    "body_base64": base64::engine::general_purpose::STANDARD.encode("console.log('ok');")
                }
            })
            .to_string(),
        );
        assert!(respond.contains("200 OK"), "{respond}");

        let browser_response = browser.join().expect("browser response");
        assert!(browser_response.contains("200 OK"), "{browser_response}");
        assert!(
            browser_response.contains("console.log('ok');"),
            "{browser_response}"
        );
    });
}

#[test]
fn route_proxy_serves_artifact_json_with_cors_headers() {
    test_support::with_isolated_home(|_| {
        let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream bind");
        let upstream_port = upstream.local_addr().expect("upstream addr").port();
        thread::spawn(move || {
            let (mut stream, _) = upstream.accept().expect("accept upstream");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone upstream"))
                .read_line(&mut request)
                .expect("read upstream request");
            assert!(
                request
                    .contains("/homeboy/workflow-bench/runs/run-1/artifacts/blueprint.after.json"),
                "{request}"
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 12\r\n\r\n{\"steps\":[]}")
                .expect("write upstream response");
        });

        let ingress = TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let ingress_port = ingress.local_addr().expect("ingress addr").port();
        thread::spawn(move || {
            serve_listener(
                PreviewIngressServeSpec {
                    bind: format!("127.0.0.1:{ingress_port}"),
                    domain: "example.com".to_string(),
                    public_host_pattern: "*-tunnel.example.com".to_string(),
                    token_sha256_env: "HOMEBOY_TEST_UNUSED_TOKEN_SHA256".to_string(),
                },
                ingress,
            )
            .expect("serve ingress");
        });
        thread::sleep(Duration::from_millis(100));

        register_route(PreviewIngressRoute {
            session_id: "run-1".to_string(),
            public_host: "run-1-tunnel.example.com".to_string(),
            upstream_origin: format!("http://127.0.0.1:{upstream_port}"),
            expires_at: None,
            active: true,
        })
        .expect("register route");

        let response = raw_http_request(
            ingress_port,
            "GET /homeboy/workflow-bench/runs/run-1/artifacts/blueprint.after.json HTTP/1.1\r\nHost: run-1-tunnel.example.com\r\n\r\n",
        );

        assert!(response.contains("200 OK"), "{response}");
        assert!(
            response.contains("access-control-allow-origin: *"),
            "{response}"
        );
        assert!(
            response.contains("content-type: application/json"),
            "{response}"
        );
        assert!(response.contains("{\"steps\":[]}"), "{response}");
    });
}

#[test]
fn route_proxy_answers_artifact_preflight_without_upstream() {
    test_support::with_isolated_home(|_| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let port = listener.local_addr().expect("local addr").port();
        thread::spawn(move || {
            serve_listener(
                PreviewIngressServeSpec {
                    bind: format!("127.0.0.1:{port}"),
                    domain: "example.com".to_string(),
                    public_host_pattern: "*-tunnel.example.com".to_string(),
                    token_sha256_env: "HOMEBOY_TEST_UNUSED_TOKEN_SHA256".to_string(),
                },
                listener,
            )
            .expect("serve ingress");
        });
        thread::sleep(Duration::from_millis(100));

        register_route(PreviewIngressRoute {
            session_id: "run-1".to_string(),
            public_host: "run-1-tunnel.example.com".to_string(),
            upstream_origin: "http://127.0.0.1:9".to_string(),
            expires_at: None,
            active: true,
        })
        .expect("register route");

        let response = raw_http_request(
            port,
            "OPTIONS /homeboy/workflow-bench/runs/run-1/artifacts/blueprint.after.json HTTP/1.1\r\nHost: run-1-tunnel.example.com\r\n\r\n",
        );

        assert!(response.contains("204 No Content"), "{response}");
        assert!(
            response.contains("access-control-allow-origin: *"),
            "{response}"
        );
        assert!(
            response.contains("access-control-allow-methods: GET, HEAD, OPTIONS"),
            "{response}"
        );
        assert!(
            response.contains("content-type: application/json"),
            "{response}"
        );
    });
}

fn http_request(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    bearer: Option<&str>,
    body: String,
) -> String {
    let auth = bearer
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    raw_http_request(
        port,
        &format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn raw_http_request(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

fn response_json(response: &str) -> serde_json::Value {
    let body = response.split("\r\n\r\n").nth(1).expect("response body");
    serde_json::from_str(body).expect("json body")
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
    assert_eq!(plan.plan.mode.as_deref(), Some("preview"));
    assert_eq!(plan.plan.policy["would_mutate"], json!(false));
    assert_eq!(plan.plan.summary.as_ref().expect("summary").ready, 8);
    assert!(plan
        .plan
        .steps
        .iter()
        .any(|step| step.id == "preview_ingress.rollback_commands"));
    assert!(plan
        .plan
        .steps
        .iter()
        .any(|step| step.id == "preview_ingress.smoke_checks"));
    assert!(plan
        .plan
        .artifacts
        .iter()
        .any(|artifact| artifact.id == "preview_ingress.systemd_unit"));

    let json = serde_json::to_value(&plan).expect("serialize install plan");
    assert_eq!(json["server_id"], "preview-vps");
    assert_eq!(json["writes"].as_array().expect("writes").len(), 3);
    assert_eq!(json["plan"]["policy"]["dry_run"], true);
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
    assert_eq!(status.plan.mode.as_deref(), Some("preview"));
    assert_eq!(status.plan.policy["would_mutate"], json!(false));
    assert_eq!(status.plan.policy["probed"], json!(false));
    assert_eq!(status.plan.summary.as_ref().expect("summary").ready, 5);
    assert_eq!(status.plan.steps.len(), status.checks.len());
    assert!(status
        .plan
        .artifacts
        .iter()
        .any(|artifact| artifact.id == "preview_ingress.status_commands"));

    let json = serde_json::to_value(&status).expect("serialize status plan");
    assert_eq!(json["checks"].as_array().expect("checks").len(), 5);
    assert_eq!(json["plan"]["summary"]["ready"], 5);
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
