use super::tunnel::*;
use crate::core::paths;
use crate::core::server::Server;
use crate::test_support;
use std::collections::{BTreeMap, HashMap};

fn create_server() {
    crate::core::server::save(&Server {
        id: "private-host".to_string(),
        aliases: Vec::new(),
        host: "private.example.test".to_string(),
        user: "tester".to_string(),
        port: 22,
        identity_file: None,
        kind: None,
        auth: None,
        env: HashMap::new(),
        runner: None,
    })
    .expect("save server");
}

#[test]
fn expose_records_private_loopback_declaration_without_running_tunnel() {
    test_support::with_isolated_home(|_| {
        create_server();

        let tunnel = expose(ExposeServiceTunnelSpec {
            id: "site-preview".to_string(),
            server_id: "private-host".to_string(),
            target: ServiceTunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 7331,
            },
            scheme: "http".to_string(),
            local_port: Some(8831),
            auth: ServiceTunnelAuth {
                mode: ServiceTunnelAuthMode::BearerEnv,
                env_var: Some("SITE_PREVIEW_TOKEN".to_string()),
                header: Some("Authorization".to_string()),
            },
            policy: ServiceTunnelPolicy {
                exposure: ServiceTunnelExposure::PrivateLoopback,
                require_auth: true,
                allowed_clients: vec!["app-runtime".to_string()],
                preview: ServiceTunnelPreviewPolicy::default(),
                native_preview_auth: ServiceTunnelNativePreviewAuthPolicy::default(),
            },
            description: Some("Private preview service".to_string()),
        })
        .expect("expose service");

        assert_eq!(tunnel.id, "site-preview");
        let report = status("site-preview").expect("status");
        assert!(report.declared);
        assert!(!report.running);
        assert_eq!(report.local_url, "http://127.0.0.1:8831");
    });
}

#[test]
fn validation_rejects_auth_mode_without_env_var() {
    test_support::with_isolated_home(|_| {
        create_server();
        let err = expose(ExposeServiceTunnelSpec {
            id: "bad".to_string(),
            server_id: "private-host".to_string(),
            target: ServiceTunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 7331,
            },
            scheme: "http".to_string(),
            local_port: None,
            auth: ServiceTunnelAuth {
                mode: ServiceTunnelAuthMode::BearerEnv,
                env_var: None,
                header: None,
            },
            policy: ServiceTunnelPolicy {
                exposure: ServiceTunnelExposure::PrivateLoopback,
                require_auth: true,
                allowed_clients: Vec::new(),
                preview: ServiceTunnelPreviewPolicy::default(),
                native_preview_auth: ServiceTunnelNativePreviewAuthPolicy::default(),
            },
            description: None,
        })
        .expect_err("missing auth env should fail");

        assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
        assert!(err.message.contains("auth.env_var"));
    });
}

#[test]
fn start_status_and_stop_manage_local_service_runtime_state() {
    test_support::with_isolated_home(|_| {
        create_server();
        expose(ExposeServiceTunnelSpec {
            id: "local-preview".to_string(),
            server_id: "private-host".to_string(),
            target: ServiceTunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 7331,
            },
            scheme: "http".to_string(),
            local_port: Some(8832),
            auth: ServiceTunnelAuth {
                mode: ServiceTunnelAuthMode::BearerEnv,
                env_var: Some("LOCAL_PREVIEW_TOKEN".to_string()),
                header: Some("Authorization".to_string()),
            },
            policy: ServiceTunnelPolicy {
                exposure: ServiceTunnelExposure::PrivateLoopback,
                require_auth: true,
                allowed_clients: vec!["app-runtime".to_string()],
                preview: ServiceTunnelPreviewPolicy {
                    mode: ServiceTunnelPreviewPolicyMode::Always,
                    keep_alive_until: None,
                },
                native_preview_auth: ServiceTunnelNativePreviewAuthPolicy::default(),
            },
            description: None,
        })
        .expect("expose service");

        let started = start(StartServiceTunnelSpec {
            id: "local-preview".to_string(),
            command: "while true; do sleep 1; done".to_string(),
            cwd: None,
            env: BTreeMap::from([("LOCAL_PREVIEW_MODE".to_string(), "test".to_string())]),
            host: Some("127.0.0.1".to_string()),
            port: Some(8832),
            scheme: Some("http".to_string()),
            health_url: None,
            health_path: None,
            readiness_timeout_secs: 1,
            backend: ServiceTunnelTunnelBackend::None,
            backend_command: None,
            backend_public_url: None,
            source_run_id: Some("run-123".to_string()),
            source_workflow_id: Some("workflow-abc".to_string()),
        })
        .expect("start service");

        assert!(started.running);
        assert_eq!(started.local_url, "http://127.0.0.1:8832");
        assert_eq!(started.preview_identity.public_url, None);
        let preview = started.preview.as_ref().expect("preview artifact");
        assert_eq!(preview.kind, "preview_url");
        assert_eq!(preview.preview_identity.service_id, "local-preview");
        assert_eq!(preview.local_url, "http://127.0.0.1:8832");
        assert_eq!(preview.source.run_id.as_deref(), Some("run-123"));
        assert_eq!(preview.source.workflow_id.as_deref(), Some("workflow-abc"));
        let process = started.process.expect("process status");
        assert!(process.running);
        assert_eq!(process.process.command.env_keys, vec!["LOCAL_PREVIEW_MODE"]);
        let evidence = started.evidence.expect("evidence paths");
        assert!(std::path::Path::new(&evidence.state_path).exists());
        assert!(std::path::Path::new(&evidence.logs.stdout_path).exists());
        assert!(std::path::Path::new(&evidence.logs.stderr_path).exists());

        let running = status("local-preview").expect("status");
        assert!(running.running);

        let stopped = stop("local-preview").expect("stop service");
        assert!(!stopped.running);
        assert!(stopped.process.is_none());
        assert!(!std::path::Path::new(&evidence.state_path).exists());
    });
}

#[test]
fn start_cleans_runtime_state_when_readiness_fails() {
    test_support::with_isolated_home(|_| {
        create_server();
        expose(ExposeServiceTunnelSpec {
            id: "failing-preview".to_string(),
            server_id: "private-host".to_string(),
            target: ServiceTunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 7331,
            },
            scheme: "http".to_string(),
            local_port: Some(8833),
            auth: ServiceTunnelAuth {
                mode: ServiceTunnelAuthMode::BearerEnv,
                env_var: Some("FAILING_PREVIEW_TOKEN".to_string()),
                header: Some("Authorization".to_string()),
            },
            policy: ServiceTunnelPolicy {
                exposure: ServiceTunnelExposure::PrivateLoopback,
                require_auth: true,
                allowed_clients: Vec::new(),
                preview: ServiceTunnelPreviewPolicy::default(),
                native_preview_auth: ServiceTunnelNativePreviewAuthPolicy::default(),
            },
            description: None,
        })
        .expect("expose service");

        let err = start(StartServiceTunnelSpec {
            id: "failing-preview".to_string(),
            command: "while true; do sleep 1; done".to_string(),
            cwd: None,
            env: BTreeMap::new(),
            host: Some("127.0.0.1".to_string()),
            port: Some(8833),
            scheme: Some("http".to_string()),
            health_url: Some("http://127.0.0.1:9/health".to_string()),
            health_path: None,
            readiness_timeout_secs: 0,
            backend: ServiceTunnelTunnelBackend::None,
            backend_command: None,
            backend_public_url: None,
            source_run_id: None,
            source_workflow_id: None,
        })
        .expect_err("readiness should fail");

        assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
        let state_path =
            paths::service_tunnel_runtime_state_file("failing-preview").expect("state path");
        assert!(!state_path.exists());
        let stopped = status("failing-preview").expect("status");
        assert!(!stopped.running);
    });
}

#[test]
fn command_backend_records_public_url_and_cleans_up_backend_process() {
    test_support::with_isolated_home(|_| {
        create_server();
        expose(ExposeServiceTunnelSpec {
            id: "command-preview".to_string(),
            server_id: "private-host".to_string(),
            target: ServiceTunnelTarget {
                host: "127.0.0.1".to_string(),
                port: 7331,
            },
            scheme: "http".to_string(),
            local_port: Some(8834),
            auth: ServiceTunnelAuth {
                mode: ServiceTunnelAuthMode::BearerEnv,
                env_var: Some("COMMAND_PREVIEW_TOKEN".to_string()),
                header: Some("Authorization".to_string()),
            },
            policy: ServiceTunnelPolicy {
                exposure: ServiceTunnelExposure::PrivateLoopback,
                require_auth: true,
                allowed_clients: Vec::new(),
                preview: ServiceTunnelPreviewPolicy::default(),
                native_preview_auth: ServiceTunnelNativePreviewAuthPolicy::default(),
            },
            description: None,
        })
        .expect("expose service");

        let started = start(StartServiceTunnelSpec {
            id: "command-preview".to_string(),
            command: "while true; do sleep 1; done".to_string(),
            cwd: None,
            env: BTreeMap::new(),
            host: Some("127.0.0.1".to_string()),
            port: Some(8834),
            scheme: Some("http".to_string()),
            health_url: None,
            health_path: None,
            readiness_timeout_secs: 1,
            backend: ServiceTunnelTunnelBackend::Command,
            backend_command: Some("while true; do sleep 1; done".to_string()),
            backend_public_url: Some("https://preview.example.test".to_string()),
            source_run_id: None,
            source_workflow_id: None,
        })
        .expect("start service with backend");

        assert!(started.running);
        assert_eq!(
            started.preview_identity.public_url.as_deref(),
            Some("https://preview.example.test")
        );
        let backend = started.tunnel_backend.expect("backend status");
        assert_eq!(backend.backend, ServiceTunnelTunnelBackend::Command);
        assert!(backend.active);
        let process = backend.process.expect("backend process");
        assert!(process.running);
        assert_eq!(
            process.process.command.env_keys,
            vec![
                "HOMEBOY_SERVICE_ID",
                "HOMEBOY_SERVICE_LOCAL_URL",
                "HOMEBOY_TUNNEL_PUBLIC_URL"
            ]
        );
        let evidence = backend.evidence.expect("backend evidence");
        assert!(std::path::Path::new(&evidence.stdout_path).exists());
        assert!(std::path::Path::new(&evidence.stderr_path).exists());

        let stopped = stop("command-preview").expect("stop service");
        assert!(!stopped.running);
        assert!(stopped.tunnel_backend.is_none());
    });
}

#[test]
fn preview_policy_decisions_match_workflow_outcomes() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-06-07T12:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    let failed = ServiceTunnelPreviewDecisionContext {
        run_failed: true,
        manual_approval_required: false,
        now,
    };
    let success = ServiceTunnelPreviewDecisionContext {
        run_failed: false,
        manual_approval_required: false,
        now,
    };
    let approval = ServiceTunnelPreviewDecisionContext {
        run_failed: false,
        manual_approval_required: true,
        now,
    };

    assert!(!preview_policy_allows(
        &ServiceTunnelPreviewPolicy::default(),
        &failed
    ));
    assert!(preview_policy_allows(
        &ServiceTunnelPreviewPolicy {
            mode: ServiceTunnelPreviewPolicyMode::Always,
            keep_alive_until: None,
        },
        &success
    ));
    assert!(preview_policy_allows(
        &ServiceTunnelPreviewPolicy {
            mode: ServiceTunnelPreviewPolicyMode::OnFailure,
            keep_alive_until: None,
        },
        &failed
    ));
    assert!(!preview_policy_allows(
        &ServiceTunnelPreviewPolicy {
            mode: ServiceTunnelPreviewPolicyMode::OnFailure,
            keep_alive_until: None,
        },
        &success
    ));
    assert!(preview_policy_allows(
        &ServiceTunnelPreviewPolicy {
            mode: ServiceTunnelPreviewPolicyMode::ManualApproval,
            keep_alive_until: None,
        },
        &approval
    ));
    assert!(preview_policy_allows(
        &ServiceTunnelPreviewPolicy {
            mode: ServiceTunnelPreviewPolicyMode::KeepAliveUntil,
            keep_alive_until: Some("2026-06-07T12:30:00Z".to_string()),
        },
        &success
    ));
    assert!(!preview_policy_allows(
        &ServiceTunnelPreviewPolicy {
            mode: ServiceTunnelPreviewPolicyMode::KeepAliveUntil,
            keep_alive_until: Some("2026-06-07T11:59:59Z".to_string()),
        },
        &success
    ));
}

fn native_preview_tunnel() -> ServiceTunnel {
    let mut token = native_preview_token_record("lab-token", "secret-token");
    token.allowed_clients = vec!["lab-client".to_string()];
    token.allowed_public_hosts = vec!["*.preview.example.test".to_string()];
    token.allowed_session_ids = vec!["run-123".to_string()];

    ServiceTunnel {
        id: "site-preview".to_string(),
        aliases: Vec::new(),
        description: None,
        server_id: "private-host".to_string(),
        target: ServiceTunnelTarget {
            host: "127.0.0.1".to_string(),
            port: 7331,
        },
        scheme: "http".to_string(),
        local_host: "127.0.0.1".to_string(),
        local_port: Some(7331),
        auth: ServiceTunnelAuth {
            mode: ServiceTunnelAuthMode::BearerEnv,
            env_var: Some("TOKEN".to_string()),
            header: Some("Authorization".to_string()),
        },
        policy: ServiceTunnelPolicy {
            exposure: ServiceTunnelExposure::PrivateLoopback,
            require_auth: true,
            allowed_clients: Vec::new(),
            preview: ServiceTunnelPreviewPolicy::default(),
            native_preview_auth: ServiceTunnelNativePreviewAuthPolicy {
                require_client_token: true,
                default_session_ttl_secs: 60,
                max_session_ttl_secs: 300,
                allowed_public_hosts: vec!["*.preview.example.test".to_string()],
                allowed_session_ids: vec!["run-123".to_string()],
                tokens: vec![token],
            },
        },
    }
}

fn native_preview_request() -> ServiceTunnelNativePreviewClaimRequest {
    ServiceTunnelNativePreviewClaimRequest {
        client_id: "lab-client".to_string(),
        token: "secret-token".to_string(),
        public_host: "run-123.preview.example.test".to_string(),
        session_id: "run-123".to_string(),
        local_origin: "http://127.0.0.1:7331".to_string(),
        requested_ttl_secs: Some(120),
        now: chrono::DateTime::parse_from_rfc3339("2026-06-07T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc),
    }
}

#[test]
fn native_preview_claim_accepts_scoped_client_host_session_and_caps_lease() {
    let tunnel = native_preview_tunnel();
    let mut request = native_preview_request();
    request.requested_ttl_secs = Some(600);

    let claim = validate_native_preview_claim(&tunnel, request).expect("valid claim");

    assert_eq!(claim.service_id, "site-preview");
    assert_eq!(claim.client_id, "lab-client");
    assert_eq!(claim.token_id, "lab-token");
    assert_eq!(claim.public_host, "run-123.preview.example.test");
    assert_eq!(claim.session_id, "run-123");
    assert_eq!(claim.local_origin, "http://127.0.0.1:7331");
    assert_eq!(claim.expires_at, "2026-06-07T12:05:00+00:00");
}

#[test]
fn native_preview_claim_rejects_wrong_token_without_leaking_expected_token() {
    let tunnel = native_preview_tunnel();
    let mut request = native_preview_request();
    request.token = "wrong-token".to_string();

    let err = validate_native_preview_claim(&tunnel, request).expect_err("wrong token fails");

    assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
    assert!(err.message.contains("not recognized"));
    assert!(!err.message.contains("secret-token"));
    assert!(!err.details.to_string().contains("secret-token"));
}

#[test]
fn native_preview_claim_rejects_wrong_client_host_session_and_origin() {
    let tunnel = native_preview_tunnel();

    let mut request = native_preview_request();
    request.client_id = "other-client".to_string();
    let err = validate_native_preview_claim(&tunnel, request).expect_err("wrong client fails");
    assert!(err.message.contains("client is not authorized"));

    let mut request = native_preview_request();
    request.public_host = "run-123.other.example.test".to_string();
    let err = validate_native_preview_claim(&tunnel, request).expect_err("wrong host fails");
    assert!(err.message.contains("public host"));

    let mut request = native_preview_request();
    request.session_id = "run-456".to_string();
    let err = validate_native_preview_claim(&tunnel, request).expect_err("wrong session fails");
    assert!(err.message.contains("session id"));

    let mut request = native_preview_request();
    request.local_origin = "http://192.168.1.5:7331".to_string();
    let err =
        validate_native_preview_claim(&tunnel, request).expect_err("non-loopback origin fails");
    assert!(err.message.contains("loopback"));
}

#[test]
fn native_preview_claim_rejects_revoked_and_expired_tokens() {
    let mut tunnel = native_preview_tunnel();
    tunnel.policy.native_preview_auth.tokens[0].revoked = true;
    let err = validate_native_preview_claim(&tunnel, native_preview_request())
        .expect_err("revoked token fails");
    assert!(err.message.contains("revoked"));

    let mut tunnel = native_preview_tunnel();
    tunnel.policy.native_preview_auth.tokens[0].expires_at =
        Some("2026-06-07T11:59:59Z".to_string());
    let err = validate_native_preview_claim(&tunnel, native_preview_request())
        .expect_err("expired token fails");
    assert!(err.message.contains("expired"));
}

#[test]
fn preview_artifact_serializes_structured_reviewer_contract() {
    let tunnel = ServiceTunnel {
        id: "site-preview".to_string(),
        aliases: Vec::new(),
        description: None,
        server_id: "private-host".to_string(),
        target: ServiceTunnelTarget {
            host: "127.0.0.1".to_string(),
            port: 3000,
        },
        scheme: "http".to_string(),
        local_host: "127.0.0.1".to_string(),
        local_port: Some(3000),
        auth: ServiceTunnelAuth {
            mode: ServiceTunnelAuthMode::BearerEnv,
            env_var: Some("TOKEN".to_string()),
            header: Some("Authorization".to_string()),
        },
        policy: ServiceTunnelPolicy {
            exposure: ServiceTunnelExposure::PrivateLoopback,
            require_auth: true,
            allowed_clients: Vec::new(),
            preview: ServiceTunnelPreviewPolicy {
                mode: ServiceTunnelPreviewPolicyMode::KeepAliveUntil,
                keep_alive_until: Some("2026-06-07T13:00:00Z".to_string()),
            },
            native_preview_auth: ServiceTunnelNativePreviewAuthPolicy::default(),
        },
    };
    let state = ServiceTunnelRuntimeState {
        preview_identity: ServiceTunnelPreviewIdentity {
            service_id: "site-preview".to_string(),
            public_url: Some("https://preview.example.test/site-preview".to_string()),
        },
        pid: 123,
        process: ServiceTunnelProcessDescriptor {
            process_group_id: Some(123),
            command: ServiceTunnelCommandSpec {
                command: "serve-app".to_string(),
                cwd: Some("/workspace/app".to_string()),
                env_keys: vec!["TOKEN".to_string()],
            },
        },
        started_at: "2026-06-07T12:00:00Z".to_string(),
        local_url: "http://127.0.0.1:3000".to_string(),
        health_url: Some("http://127.0.0.1:3000/".to_string()),
        logs: ServiceTunnelLogPaths {
            stdout_path: "/tmp/homeboy/stdout.log".to_string(),
            stderr_path: "/tmp/homeboy/stderr.log".to_string(),
        },
        backend: ServiceTunnelTunnelBackend::None,
        backend_process: None,
        source_run_id: Some("run-1".to_string()),
        source_workflow_id: Some("workflow-1".to_string()),
    };
    let context = ServiceTunnelPreviewDecisionContext {
        run_failed: false,
        manual_approval_required: false,
        now: chrono::DateTime::parse_from_rfc3339("2026-06-07T12:30:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc),
    };

    let artifact = preview_artifact_for(&tunnel, &state, &context).expect("artifact");
    let serialized = serde_json::to_value(&artifact).expect("serialize artifact");
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/output_contracts/tunnel/preview-artifact.json"
    ))
    .expect("fixture");

    assert_eq!(serialized, expected);
    assert_eq!(serialized["schema"], "homeboy/preview-url/v1");
    assert_eq!(serialized["kind"], "preview_url");
    assert_eq!(serialized["service_id"], "site-preview");
    assert_eq!(serialized["local_url"], "http://127.0.0.1:3000");
    assert_eq!(
        serialized["public_url"],
        "https://preview.example.test/site-preview"
    );
    assert_eq!(serialized["backend"], "none");
    assert_eq!(serialized["policy"]["mode"], "keep_alive_until");
    assert_eq!(serialized["cleanup"]["expires_at"], "2026-06-07T13:00:00Z");
    assert_eq!(serialized["source"]["run_id"], "run-1");
    assert_eq!(serialized["source"]["workflow_id"], "workflow-1");
}
