use super::*;

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
        const OUTPUT_LIMIT_BYTES: u64 = 64 * 1024;
        let mut response = String::new();
        stream
            .take(OUTPUT_LIMIT_BYTES)
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
