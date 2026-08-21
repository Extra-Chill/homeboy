use super::preview_ingress::*;
use crate::native_preview_token_sha256;
use crate::preview_client::{self, PreviewClientStartSpec};
use base64::Engine;
use homeboy_core::test_support;
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn route_registers_host_to_upstream_origin() {
    test_support::with_isolated_home(|_| {
        let route = register_route(PreviewIngressRoute {
            session_id: "run-123".to_string(),
            public_host: "run-123-tunnel.preview.example.test".to_string(),
            upstream_origin: "http://127.0.0.1:7331".to_string(),
            expires_at: None,
            active: true,
        })
        .expect("register route");

        assert_eq!(route.session_id, "run-123");
        let status = status(
            Some("127.0.0.1:7350".to_string()),
            Some("preview.example.test".to_string()),
            Some("*-tunnel.preview.example.test".to_string()),
        )
        .expect("status");
        assert_eq!(status.routes.len(), 1);
        assert_eq!(
            status.routes[0].lifecycle,
            PreviewIngressRouteLifecycle::Active
        );
        assert_eq!(
            status.routes[0].route.public_host,
            "run-123-tunnel.preview.example.test"
        );
    });
}

#[test]
fn route_status_reports_expired_and_disconnected_sessions() {
    test_support::with_isolated_home(|_| {
        register_route(PreviewIngressRoute {
            session_id: "expired".to_string(),
            public_host: "expired-tunnel.preview.example.test".to_string(),
            upstream_origin: "http://127.0.0.1:7331".to_string(),
            expires_at: Some("2000-01-01T00:00:00Z".to_string()),
            active: true,
        })
        .expect("register expired route");
        register_route(PreviewIngressRoute {
            session_id: "disconnected".to_string(),
            public_host: "disconnected-tunnel.preview.example.test".to_string(),
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
            public_host: "run-123-tunnel.preview.example.test".to_string(),
            upstream_origin: "http://127.0.0.1:7331".to_string(),
            expires_at: None,
            active: true,
        })
        .expect("register route");

        let status = status_for_host(
            None,
            None,
            None,
            Some("RUN-123-TUNNEL.PREVIEW.EXAMPLE.TEST:443".to_string()),
        )
        .expect("status");

        assert_eq!(
            status.inspected_host.as_deref(),
            Some("run-123-tunnel.preview.example.test")
        );
        assert_eq!(status.inspected_state.as_deref(), Some("registered"));
    });
}

#[test]
fn route_validation_rejects_non_http_upstream_origin() {
    test_support::with_isolated_home(|_| {
        let err = register_route(PreviewIngressRoute {
            session_id: "bad".to_string(),
            public_host: "bad-tunnel.preview.example.test".to_string(),
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

/// Boot an ingress on an ephemeral port and return that port.
///
/// Each caller passes its own env var name so tests never share auth state.
fn spawn_ingress(token_sha256_env: &str, public_host_pattern: &str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve port");
    let port = listener.local_addr().expect("local addr").port();
    let token_sha256_env = token_sha256_env.to_string();
    let public_host_pattern = public_host_pattern.to_string();
    thread::spawn(move || {
        serve_listener(
            PreviewIngressServeSpec {
                bind: format!("127.0.0.1:{port}"),
                domain: "example.com".to_string(),
                public_host_pattern,
                token_sha256_env,
            },
            listener,
        )
        .expect("serve ingress");
    });
    thread::sleep(Duration::from_millis(100));
    port
}

fn register_body(public_host: &str) -> String {
    json!({
        "public_host": public_host,
        "local_origin": "http://127.0.0.1:49999",
        "session_id": "run-1"
    })
    .to_string()
}

fn register_channel(port: u16, token: &str, public_host: &str) -> String {
    let response = http_request(
        port,
        "POST",
        "/preview/client/register",
        "homeboy-health-tunnel.example.com",
        Some(token),
        register_body(public_host),
    );
    assert!(response.contains("200 OK"), "{response}");
    response_json(&response)["channel_id"]
        .as_str()
        .expect("registration channel id")
        .to_string()
}

#[test]
fn reverse_channel_websocket_four_hop_round_trips_frames_and_close() {
    test_support::with_isolated_home(|_| {
        use tungstenite::client::IntoClientRequest;
        use tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
        use tungstenite::Message;

        let token = "websocket-four-hop-token";
        std::env::set_var("HOMEBOY_TEST_WEBSOCKET_TOKEN", token);
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );

        let origin = TcpListener::bind("127.0.0.1:0").expect("bind WebSocket origin");
        let origin_port = origin.local_addr().expect("origin address").port();
        let (origin_events_tx, origin_events_rx) = mpsc::channel();
        let origin_thread = thread::spawn(move || {
            let (stream, _) = origin.accept().expect("accept WebSocket origin connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set origin read timeout");
            let mut socket = tungstenite::accept(stream).expect("accept WebSocket handshake");
            loop {
                match socket.read().expect("read origin WebSocket frame") {
                    Message::Text(text) => {
                        socket.send(Message::Text(text)).expect("echo origin text")
                    }
                    Message::Binary(bytes) => socket
                        .send(Message::Binary(bytes))
                        .expect("echo origin binary"),
                    Message::Ping(bytes) => {
                        origin_events_tx
                            .send(("ping".to_string(), bytes.to_vec()))
                            .expect("record origin ping");
                        socket
                            .send(Message::Pong(b"origin-pong".to_vec().into()))
                            .expect("reply to origin ping");
                        socket
                            .send(Message::Close(Some(CloseFrame {
                                code: CloseCode::Policy,
                                reason: "four-hop-finished".into(),
                            })))
                            .expect("send origin close");
                        let reply = socket
                            .read()
                            .expect("read propagated origin close acknowledgement");
                        let Message::Close(Some(reply)) = reply else {
                            panic!("expected origin close acknowledgement, got {reply:?}");
                        };
                        assert_eq!(reply.code, CloseCode::Policy);
                        assert_eq!(reply.reason, "four-hop-finished");
                        break;
                    }
                    Message::Close(frame) => panic!("origin received unexpected close: {frame:?}"),
                    other => panic!("unexpected origin frame: {other:?}"),
                }
            }
        });

        let ingress = TcpListener::bind("127.0.0.1:0").expect("bind WebSocket ingress");
        let ingress_port = ingress.local_addr().expect("ingress address").port();
        thread::spawn(move || {
            serve_listener(
                PreviewIngressServeSpec {
                    bind: format!("127.0.0.1:{ingress_port}"),
                    domain: "example.com".to_string(),
                    public_host_pattern: "*-tunnel.example.com".to_string(),
                    token_sha256_env: "HOMEBOY_TEST_WEBSOCKET_TOKEN_SHA256".to_string(),
                },
                ingress,
            )
            .expect("serve WebSocket ingress");
        });

        let stop = Arc::new(AtomicBool::new(false));
        let client_stop = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let client_thread = thread::spawn(move || {
            preview_client::supervise_with_ready(
                PreviewClientStartSpec {
                    ingress: format!("http://127.0.0.1:{ingress_port}"),
                    public_host: "websocket-tunnel.example.com".to_string(),
                    local_origin: format!("http://127.0.0.1:{origin_port}"),
                    session_id: Some("websocket-four-hop".to_string()),
                    token_env: "HOMEBOY_TEST_WEBSOCKET_TOKEN".to_string(),
                    poll_timeout_secs: 1,
                    ready_stdout: false,
                },
                client_stop,
                move || ready_tx.send(()).expect("signal preview client ready"),
            )
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("preview client readiness");

        let tcp = TcpStream::connect(("127.0.0.1", ingress_port))
            .expect("connect public WebSocket client");
        tcp.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set public read timeout");
        tcp.set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set public write timeout");
        let mut request = format!("ws://127.0.0.1:{ingress_port}/echo?four=hop")
            .into_client_request()
            .expect("build public WebSocket request");
        request.headers_mut().insert(
            tungstenite::http::header::HOST,
            "websocket-tunnel.example.com"
                .parse()
                .expect("public Host header"),
        );
        let (mut public, response) =
            tungstenite::client(request, tcp).expect("public WebSocket handshake");
        assert_eq!(response.status(), 101);

        public
            .send(Message::Text("hello over four hops".into()))
            .expect("send public text");
        assert_eq!(
            public.read().expect("read echoed text"),
            Message::Text("hello over four hops".into())
        );

        let binary = vec![0, 1, 2, 127, 128, 255];
        public
            .send(Message::Binary(binary.clone().into()))
            .expect("send public binary");
        assert_eq!(
            public.read().expect("read echoed binary"),
            Message::Binary(binary.into())
        );

        let ping = b"four-hop-ping".to_vec();
        public
            .send(Message::Ping(ping.clone().into()))
            .expect("send public ping");
        assert_eq!(
            origin_events_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("origin ping observation"),
            ("ping".to_string(), ping)
        );

        let mut saw_origin_pong = false;
        let mut propagated_close = None;
        for _ in 0..4 {
            match public.read().expect("read propagated control frame") {
                Message::Pong(payload) if payload.as_ref() == b"origin-pong" => {
                    saw_origin_pong = true;
                }
                Message::Pong(_) => {}
                Message::Close(frame) => {
                    propagated_close = frame;
                    break;
                }
                other => panic!("unexpected public control frame: {other:?}"),
            }
        }
        assert!(saw_origin_pong, "origin pong did not reach public client");
        let close = propagated_close.expect("propagated close frame");
        assert_eq!(close.code, CloseCode::Policy);
        assert_eq!(close.reason, "four-hop-finished");

        origin_thread.join().expect("WebSocket origin completed");
        stop.store(true, Ordering::SeqCst);
        client_thread
            .join()
            .expect("preview client completed")
            .expect("preview client clean shutdown");
    });
}

#[test]
fn reverse_channel_websocket_public_close_waits_for_delayed_origin_acknowledgement() {
    test_support::with_isolated_home(|_| {
        use tungstenite::client::IntoClientRequest;
        use tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
        use tungstenite::Message;

        let token = "websocket-public-close-token";
        std::env::set_var("HOMEBOY_TEST_WEBSOCKET_PUBLIC_CLOSE_TOKEN", token);
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_PUBLIC_CLOSE_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let origin = TcpListener::bind("127.0.0.1:0").expect("bind WebSocket origin");
        let origin_port = origin.local_addr().expect("origin address").port();
        let origin_thread = thread::spawn(move || {
            let (stream, _) = origin.accept().expect("accept WebSocket origin connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set origin read timeout");
            let mut socket = tungstenite::accept(stream).expect("accept WebSocket handshake");
            thread::sleep(Duration::from_millis(200));
            let Message::Close(Some(close)) = socket.read().expect("read public close") else {
                panic!("origin did not receive a close frame");
            };
            assert_eq!(close.code, CloseCode::Away);
            assert_eq!(close.reason, "public-close");
        });
        let ingress = TcpListener::bind("127.0.0.1:0").expect("bind ingress");
        let ingress_port = ingress.local_addr().expect("ingress address").port();
        thread::spawn(move || {
            serve_listener(
                PreviewIngressServeSpec {
                    bind: format!("127.0.0.1:{ingress_port}"),
                    domain: "example.com".to_string(),
                    public_host_pattern: "*-tunnel.example.com".to_string(),
                    token_sha256_env: "HOMEBOY_TEST_WEBSOCKET_PUBLIC_CLOSE_TOKEN_SHA256"
                        .to_string(),
                },
                ingress,
            )
            .expect("serve ingress");
        });
        let stop = Arc::new(AtomicBool::new(false));
        let client_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = mpsc::channel();
        let client_thread = thread::spawn(move || {
            preview_client::supervise_with_ready(
                PreviewClientStartSpec {
                    ingress: format!("http://127.0.0.1:{ingress_port}"),
                    public_host: "public-close-tunnel.example.com".to_string(),
                    local_origin: format!("http://127.0.0.1:{origin_port}"),
                    session_id: Some("public-close".to_string()),
                    token_env: "HOMEBOY_TEST_WEBSOCKET_PUBLIC_CLOSE_TOKEN".to_string(),
                    poll_timeout_secs: 1,
                    ready_stdout: false,
                },
                client_stop,
                move || ready_tx.send(()).expect("ready"),
            )
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("ready");
        let tcp = TcpStream::connect(("127.0.0.1", ingress_port)).expect("connect public client");
        tcp.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        let mut request = format!("ws://127.0.0.1:{ingress_port}/close")
            .into_client_request()
            .expect("build public request");
        request.headers_mut().insert(
            tungstenite::http::header::HOST,
            "public-close-tunnel.example.com".parse().expect("host"),
        );
        let (mut public, _) = tungstenite::client(request, tcp).expect("public handshake");
        public
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Away,
                reason: "public-close".into(),
            })))
            .expect("send public close");
        let Message::Close(Some(close)) = public.read().expect("read close response") else {
            panic!("public client did not receive a close frame");
        };
        assert_eq!(close.code, CloseCode::Away);
        assert_eq!(close.reason, "public-close");
        origin_thread.join().expect("origin completed");
        stop.store(true, Ordering::SeqCst);
        client_thread
            .join()
            .expect("client completed")
            .expect("client stopped");
    });
}

#[test]
fn websocket_endpoints_require_authentication() {
    test_support::with_isolated_home(|_| {
        let token = "websocket-auth-token";
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_AUTH_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_WEBSOCKET_AUTH_TOKEN_SHA256",
            "*-tunnel.example.com",
        );

        for endpoint in [
            "/preview/client/websocket/open",
            "/preview/client/websocket/next",
            "/preview/client/websocket/frame",
            "/preview/client/websocket/abort",
            "/preview/client/websocket/delivered",
        ] {
            let response = http_request(
                port,
                "POST",
                endpoint,
                "homeboy-health-tunnel.example.com",
                None,
                "{}".to_string(),
            );
            assert!(
                response.contains("401 Unauthorized"),
                "{endpoint} accepted an unauthenticated request: {response}"
            );
        }
    });
}

#[test]
fn websocket_endpoints_reject_wrong_channel() {
    test_support::with_isolated_home(|_| {
        let token = "websocket-route-owner-token";
        let public_host = "owner-tunnel.example.com";
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_OWNER_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_WEBSOCKET_OWNER_TOKEN_SHA256",
            "*-tunnel.example.com",
        );
        let channel_id = register_channel(port, token, public_host);
        assert_ne!(channel_id, "wrong-channel");

        let requests = [
            (
                "/preview/client/websocket/open",
                json!({
                    "public_host": public_host,
                    "channel_id": "wrong-channel",
                    "result": {
                        "websocket_id": "isolated-websocket",
                        "accepted": true,
                        "status": 101
                    }
                }),
            ),
            (
                "/preview/client/websocket/next",
                json!({
                    "public_host": public_host,
                    "channel_id": "wrong-channel",
                    "websocket_id": "isolated-websocket",
                    "timeout_ms": 1
                }),
            ),
            (
                "/preview/client/websocket/frame",
                json!({
                    "public_host": public_host,
                    "channel_id": "wrong-channel",
                    "frame": {
                        "websocket_id": "isolated-websocket",
                        "sequence": 0,
                        "kind": "text",
                        "payload_base64": "aXNvbGF0ZWQ="
                    }
                }),
            ),
        ];
        for (endpoint, body) in requests {
            let response = http_request(
                port,
                "POST",
                endpoint,
                "homeboy-health-tunnel.example.com",
                Some(token),
                body.to_string(),
            );
            assert!(response.contains("403 Forbidden"), "{endpoint}: {response}");
            assert!(
                response.contains("route_owner_mismatch"),
                "{endpoint}: {response}"
            );
        }
    });
}

#[test]
fn websocket_frame_endpoint_rejects_oversized_frame() {
    test_support::with_isolated_home(|_| {
        let token = "websocket-frame-limit-token";
        let public_host = "frame-limit-tunnel.example.com";
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_FRAME_LIMIT_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_WEBSOCKET_FRAME_LIMIT_TOKEN_SHA256",
            "*-tunnel.example.com",
        );
        let channel_id = register_channel(port, token, public_host);
        let payload = base64::engine::general_purpose::STANDARD.encode(vec![
            0_u8;
            PREVIEW_WEBSOCKET_MAX_FRAME_BYTES
                + 1
        ]);
        let response = http_request(
            port,
            "POST",
            "/preview/client/websocket/frame",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({
                "public_host": public_host,
                "channel_id": channel_id,
                "frame": {
                    "websocket_id": "oversized-websocket",
                    "sequence": 0,
                    "kind": "binary",
                    "payload_base64": payload
                }
            })
            .to_string(),
        );

        assert!(response.starts_with("HTTP/1.1 413 "), "{response}");
        assert!(response.contains("websocket_frame_too_large"), "{response}");
    });
}

#[test]
fn websocket_frame_endpoint_accepts_exact_limit_and_rejects_limit_plus_one() {
    test_support::with_isolated_home(|_| {
        let token = "websocket-exact-limit-token";
        let public_host = "exact-limit-tunnel.example.com";
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_EXACT_LIMIT_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_WEBSOCKET_EXACT_LIMIT_TOKEN_SHA256",
            "*-tunnel.example.com",
        );
        let channel_id = register_channel(port, token, public_host);
        for (name, bytes, expected_status) in [
            ("exact", PREVIEW_WEBSOCKET_MAX_FRAME_BYTES, "404 Not Found"),
            ("plus-one", PREVIEW_WEBSOCKET_MAX_FRAME_BYTES + 1, "413 OK"),
        ] {
            let response = http_request(
                port,
                "POST",
                "/preview/client/websocket/frame",
                "homeboy-health-tunnel.example.com",
                Some(token),
                json!({
                    "public_host": public_host,
                    "channel_id": channel_id,
                    "frame": {
                        "websocket_id": name,
                        "sequence": 0,
                        "kind": "binary",
                        "payload_base64": base64::engine::general_purpose::STANDARD.encode(vec![0_u8; bytes])
                    }
                }).to_string(),
            );
            assert!(response.contains(expected_status), "{name}: {response}");
        }
    });
}

#[test]
fn websocket_abort_requires_owner_and_frees_session_slot() {
    test_support::with_isolated_home(|_| {
        let token = "websocket-abort-token";
        let public_host = "abort-tunnel.example.com";
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_ABORT_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_WEBSOCKET_ABORT_TOKEN_SHA256",
            "*-tunnel.example.com",
        );
        let channel_id = register_channel(port, token, public_host);
        let wrong = http_request(
            port,
            "POST",
            "/preview/client/websocket/abort",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({ "public_host": public_host, "channel_id": "wrong", "websocket_id": "missing" })
                .to_string(),
        );
        assert!(wrong.contains("403 Forbidden"), "{wrong}");
        let missing = http_request(port, "POST", "/preview/client/websocket/abort", "homeboy-health-tunnel.example.com", Some(token), json!({ "public_host": public_host, "channel_id": channel_id, "websocket_id": "missing" }).to_string());
        assert!(missing.contains("404 Not Found"), "{missing}");
    });
}

#[test]
fn websocket_handshake_timeout_cleans_up_and_ingress_remains_responsive() {
    test_support::with_isolated_home(|_| {
        let token = "websocket-timeout-token";
        let public_host = "timeout-tunnel.example.com";
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_TIMEOUT_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_WEBSOCKET_TIMEOUT_TOKEN_SHA256",
            "*-tunnel.example.com",
        );
        let channel_id = register_channel(port, token, public_host);
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect public socket");
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("set timeout");
        stream
            .write_all(websocket_upgrade_request(public_host).as_bytes())
            .expect("write upgrade");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read diagnostic");
        assert!(response.contains("504"), "{response}");

        let next = http_request(
            port,
            "POST",
            "/preview/client/next",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({ "public_host": public_host, "channel_id": channel_id, "timeout_secs": 1 })
                .to_string(),
        );
        assert_eq!(response_json(&next)["websocket"], serde_json::Value::Null);
    });
}

#[test]
fn websocket_local_handshake_rejection_cleans_up_and_ingress_remains_responsive() {
    test_support::with_isolated_home(|_| {
        let token = "websocket-rejection-token";
        let public_host = "rejection-tunnel.example.com";
        std::env::set_var("HOMEBOY_TEST_WEBSOCKET_REJECTION_TOKEN", token);
        std::env::set_var(
            "HOMEBOY_TEST_WEBSOCKET_REJECTION_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let origin = TcpListener::bind("127.0.0.1:0").expect("bind rejecting origin");
        let origin_port = origin.local_addr().expect("origin address").port();
        let origin_thread = thread::spawn(move || {
            let (mut stream, _) = origin.accept().expect("accept local handshake");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone local handshake"))
                .read_line(&mut request)
                .expect("read local handshake");
            stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("reject local handshake");
        });
        let port = spawn_ingress(
            "HOMEBOY_TEST_WEBSOCKET_REJECTION_TOKEN_SHA256",
            "*-tunnel.example.com",
        );
        let stop = Arc::new(AtomicBool::new(false));
        let client_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = mpsc::channel();
        let client_thread = thread::spawn(move || {
            preview_client::supervise_with_ready(
                PreviewClientStartSpec {
                    ingress: format!("http://127.0.0.1:{port}"),
                    public_host: public_host.to_string(),
                    local_origin: format!("http://127.0.0.1:{origin_port}"),
                    session_id: Some("rejection".to_string()),
                    token_env: "HOMEBOY_TEST_WEBSOCKET_REJECTION_TOKEN".to_string(),
                    poll_timeout_secs: 1,
                    ready_stdout: false,
                },
                client_stop,
                move || ready_tx.send(()).expect("ready"),
            )
        });
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("ready");
        let response = raw_http_request(port, &websocket_upgrade_request(public_host));
        assert!(response.contains("403"), "{response}");
        origin_thread.join().expect("origin completed");

        let next = http_request(
            port,
            "POST",
            "/preview/client/next",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({ "public_host": public_host, "timeout_secs": 1 }).to_string(),
        );
        assert_eq!(response_json(&next)["websocket"], serde_json::Value::Null);
        stop.store(true, Ordering::SeqCst);
        client_thread
            .join()
            .expect("client completed")
            .expect("client stopped");
    });
}

fn websocket_upgrade_request(host: &str) -> String {
    format!(
        "GET /socket HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
}

#[test]
fn client_auth_denies_every_request_when_token_env_is_unset() {
    test_support::with_isolated_home(|_| {
        // Deliberately never set this env var: an ingress with no configured
        // digest must reject clients, not admit them.
        std::env::remove_var("HOMEBOY_TEST_UNSET_TOKEN_SHA256");
        let port = spawn_ingress("HOMEBOY_TEST_UNSET_TOKEN_SHA256", "*-tunnel.example.com");

        let with_token = http_request(
            port,
            "POST",
            "/preview/client/register",
            "homeboy-health-tunnel.example.com",
            Some("any-token-at-all"),
            register_body("run-1-tunnel.example.com"),
        );
        assert!(
            with_token.contains("401 Unauthorized"),
            "unset token env must fail closed, got: {with_token}"
        );

        let without_token = http_request(
            port,
            "POST",
            "/preview/client/register",
            "homeboy-health-tunnel.example.com",
            None,
            register_body("run-1-tunnel.example.com"),
        );
        assert!(
            without_token.contains("401 Unauthorized"),
            "unset token env must fail closed, got: {without_token}"
        );
    });
}

#[test]
fn client_auth_denies_wrong_bearer_token() {
    test_support::with_isolated_home(|_| {
        std::env::set_var(
            "HOMEBOY_TEST_WRONG_TOKEN_SHA256",
            native_preview_token_sha256("correct-token"),
        );
        let port = spawn_ingress("HOMEBOY_TEST_WRONG_TOKEN_SHA256", "*-tunnel.example.com");

        let response = http_request(
            port,
            "POST",
            "/preview/client/register",
            "homeboy-health-tunnel.example.com",
            Some("wrong-token"),
            register_body("run-1-tunnel.example.com"),
        );
        assert!(response.contains("401 Unauthorized"), "{response}");
    });
}

#[test]
fn client_with_valid_token_cannot_claim_host_outside_ingress_pattern() {
    test_support::with_isolated_home(|_| {
        let token = "host-claim-token";
        std::env::set_var(
            "HOMEBOY_TEST_HOST_CLAIM_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_HOST_CLAIM_TOKEN_SHA256",
            "*-tunnel.example.com",
        );

        // A valid token holder attempting to claim a host this ingress has no
        // authority over: the takeover primitive the bearer check alone allowed.
        for hijack in [
            "victim.other-tenant.com",
            "run-1-tunnel.evil.com",
            "example.com",
        ] {
            let response = http_request(
                port,
                "POST",
                "/preview/client/register",
                "homeboy-health-tunnel.example.com",
                Some(token),
                register_body(hijack),
            );
            assert!(
                response.contains("403 Forbidden"),
                "claiming {hijack} must be rejected, got: {response}"
            );
            assert!(
                response.contains("host_claim_rejected"),
                "claiming {hijack} must be classified, got: {response}"
            );
        }

        // The in-pattern host still registers: only added terms, nothing weakened.
        let allowed = http_request(
            port,
            "POST",
            "/preview/client/register",
            "homeboy-health-tunnel.example.com",
            Some(token),
            register_body("run-1-tunnel.example.com"),
        );
        assert!(allowed.contains("200 OK"), "{allowed}");
    });
}

#[test]
fn host_claim_is_authorized_case_insensitively_and_ignores_port() {
    test_support::with_isolated_home(|_| {
        let token = "host-normalize-token";
        std::env::set_var(
            "HOMEBOY_TEST_HOST_NORMALIZE_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let port = spawn_ingress(
            "HOMEBOY_TEST_HOST_NORMALIZE_TOKEN_SHA256",
            "*-tunnel.example.com",
        );

        // normalize_public_host lowercases and strips the port before the
        // authority check, so this must be treated as the in-pattern host.
        let response = http_request(
            port,
            "POST",
            "/preview/client/register",
            "homeboy-health-tunnel.example.com",
            Some(token),
            register_body("RUN-1-TUNNEL.EXAMPLE.COM:443"),
        );
        assert!(response.contains("200 OK"), "{response}");
    });
}

#[test]
fn durable_reverse_client_reregisters_after_session_disconnect() {
    test_support::with_isolated_home(|_| {
        let token = "durable-artifact-token";
        std::env::set_var("HOMEBOY_TEST_DURABLE_ARTIFACT_TOKEN", token);
        std::env::set_var(
            "HOMEBOY_TEST_DURABLE_ARTIFACT_TOKEN_SHA256",
            native_preview_token_sha256(token),
        );
        let origin = TcpListener::bind("127.0.0.1:0").expect("bind artifact origin");
        let origin_port = origin.local_addr().expect("origin address").port();
        thread::spawn(move || loop {
            let (mut stream, _) = origin.accept().expect("accept artifact request");
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).expect("read artifact request");
            assert!(String::from_utf8_lossy(&request[..read]).contains("/artifacts/proof.png"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 8\r\n\r\nPNGproof")
                .expect("write artifact");
        });

        let ingress = TcpListener::bind("127.0.0.1:0").expect("reserve ingress port");
        let ingress_port = ingress.local_addr().expect("ingress address").port();
        thread::spawn(move || {
            serve_listener(
                PreviewIngressServeSpec {
                    bind: format!("127.0.0.1:{ingress_port}"),
                    domain: "example.com".to_string(),
                    public_host_pattern: "*-tunnel.example.com".to_string(),
                    token_sha256_env: "HOMEBOY_TEST_DURABLE_ARTIFACT_TOKEN_SHA256".to_string(),
                },
                ingress,
            )
            .expect("serve ingress");
        });
        thread::sleep(Duration::from_millis(100));

        let stop = Arc::new(AtomicBool::new(false));
        let client_stop = stop.clone();
        let ready_count = Arc::new(AtomicUsize::new(0));
        let client_ready_count = ready_count.clone();
        let client = thread::spawn(move || {
            preview_client::supervise_with_ready(
                PreviewClientStartSpec {
                    ingress: format!("http://127.0.0.1:{ingress_port}"),
                    public_host: "artifacts-tunnel.example.com".to_string(),
                    local_origin: format!("http://127.0.0.1:{origin_port}"),
                    session_id: Some("artifact-origin".to_string()),
                    token_env: "HOMEBOY_TEST_DURABLE_ARTIFACT_TOKEN".to_string(),
                    poll_timeout_secs: 1,
                    ready_stdout: false,
                },
                client_stop,
                move || {
                    client_ready_count.fetch_add(1, Ordering::SeqCst);
                },
            )
        });

        let artifact_request = || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                let browser = thread::spawn(move || {
                    raw_http_request(
                        ingress_port,
                        "GET /artifacts/proof.png HTTP/1.1\r\nHost: artifacts-tunnel.example.com\r\n\r\n",
                    )
                });
                let response = browser.join().expect("artifact response");
                if !response.contains("missing_session") || std::time::Instant::now() >= deadline {
                    break response;
                }
                thread::sleep(Duration::from_millis(25));
            }
        };
        let first = artifact_request();
        assert!(
            first.contains("200 OK") && first.ends_with("PNGproof"),
            "{first}"
        );
        assert_eq!(ready_count.load(Ordering::SeqCst), 1);

        let closed = http_request(
            ingress_port,
            "POST",
            "/preview/client/close",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({ "public_host": "artifacts-tunnel.example.com" }).to_string(),
        );
        assert!(closed.contains("200 OK"), "{closed}");
        let reconnected = artifact_request();
        assert!(
            reconnected.contains("200 OK") && reconnected.ends_with("PNGproof"),
            "{reconnected}"
        );
        assert_eq!(ready_count.load(Ordering::SeqCst), 1);

        stop.store(true, Ordering::SeqCst);
        client
            .join()
            .expect("supervisor completed")
            .expect("clean shutdown");
    });
}

#[test]
fn reverse_channel_client_forwards_bootstrap_redirect_and_repeated_cookies() {
    test_support::with_isolated_home(|_| {
        let token = "test-preview-token";
        let expected_status_line = "HTTP/1.1 302 Found";
        let expected_location = "/wp-admin/";
        let expected_cookie_count = 2;
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
                "GET /__runtime/reviewer-auth-bootstrap?token=fake HTTP/1.1\r\nHost: run-1-tunnel.example.com\r\n\r\n",
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
        assert!(
            next.contains("/__runtime/reviewer-auth-bootstrap?token=fake"),
            "{next}"
        );
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
                    "status": 302,
                    "headers": [
                        ["location", expected_location],
                        ["set-cookie", "reviewer_auth=fake; Path=/; HttpOnly"],
                        ["set-cookie", "reviewer_test_cookie=fake; Path=/"]
                    ],
                    "body_base64": base64::engine::general_purpose::STANDARD.encode("")
                }
            })
            .to_string(),
        );
        assert!(respond.contains("200 OK"), "{respond}");

        let browser_response = browser.join().expect("browser response");
        assert!(
            browser_response.starts_with(expected_status_line),
            "{browser_response}"
        );
        assert_eq!(
            response_header_values(&browser_response, "location"),
            vec![expected_location.to_string()]
        );
        assert_eq!(
            response_header_values(&browser_response, "set-cookie").len(),
            expected_cookie_count
        );
        assert!(
            browser_response.contains("content-length: 0"),
            "{browser_response}"
        );
    });
}

#[test]
fn reverse_channel_streams_multi_megabyte_public_body() {
    test_support::with_isolated_home(|_| {
        let token = "test-preview-token-stream";
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
                "public_host": "run-stream-tunnel.example.com",
                "local_origin": "http://127.0.0.1:49999",
                "session_id": "run-stream"
            })
            .to_string(),
        );
        assert!(register.contains("200 OK"), "{register}");

        let browser = thread::spawn(move || {
            raw_http_request_bytes(
                port,
                "GET /homeboy/workflow-bench/runs/run/artifacts/blueprint.zip HTTP/1.1\r\nHost: run-stream-tunnel.example.com\r\n\r\n",
            )
        });
        thread::sleep(Duration::from_millis(100));

        let next = http_request(
            port,
            "POST",
            "/preview/client/next",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({ "public_host": "run-stream-tunnel.example.com", "timeout_secs": 2 })
                .to_string(),
        );
        assert!(next.contains("blueprint.zip"), "{next}");
        let request_id = response_json(&next)["request"]["request_id"]
            .as_str()
            .expect("request id")
            .to_string();

        let payload = vec![b'x'; (2 * 1024 * 1024) + 123];
        let respond = http_request(
            port,
            "POST",
            "/preview/client/respond",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({
                "public_host": "run-stream-tunnel.example.com",
                "response": {
                    "request_id": request_id,
                    "status": 200,
                    "headers": [
                        ["content-type", "application/zip"],
                        ["content-length", payload.len().to_string()],
                        ["access-control-allow-origin", "*"]
                    ],
                    "body_stream": true
                }
            })
            .to_string(),
        );
        assert!(respond.contains("200 OK"), "{respond}");

        for (sequence, chunk) in payload.chunks(64 * 1024).enumerate() {
            let response = http_request(
                port,
                "POST",
                "/preview/client/respond-chunk",
                "homeboy-health-tunnel.example.com",
                Some(token),
                json!({
                    "public_host": "run-stream-tunnel.example.com",
                    "chunk": {
                        "request_id": request_id,
                        "sequence": sequence,
                        "body_base64": base64::engine::general_purpose::STANDARD.encode(chunk),
                        "complete": false
                    }
                })
                .to_string(),
            );
            assert!(response.contains("200 OK"), "{response}");
        }
        let complete = http_request(
            port,
            "POST",
            "/preview/client/respond-chunk",
            "homeboy-health-tunnel.example.com",
            Some(token),
            json!({
                "public_host": "run-stream-tunnel.example.com",
                "chunk": {
                    "request_id": request_id,
                    "sequence": payload.len() / (64 * 1024) + 1,
                    "body_base64": "",
                    "complete": true
                }
            })
            .to_string(),
        );
        assert!(complete.contains("200 OK"), "{complete}");

        let browser_response = browser.join().expect("browser response");
        assert!(
            String::from_utf8_lossy(&browser_response).contains("200 OK"),
            "{}",
            String::from_utf8_lossy(&browser_response[..browser_response.len().min(256)])
        );
        assert!(
            String::from_utf8_lossy(&browser_response).contains("access-control-allow-origin: *")
        );
        assert_eq!(response_body_bytes(&browser_response), payload.as_slice());
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
    String::from_utf8(raw_http_request_bytes(port, request)).expect("utf8 response")
}

fn raw_http_request_bytes(port: u16, request: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn response_body_bytes(response: &[u8]) -> &[u8] {
    response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| &response[index + 4..])
        .expect("response body")
}

fn response_header_values(response: &str, header_name: &str) -> Vec<String> {
    response
        .split("\r\n\r\n")
        .next()
        .unwrap_or(response)
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case(header_name))
        .map(|(_, value)| value.trim().to_string())
        .collect()
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
