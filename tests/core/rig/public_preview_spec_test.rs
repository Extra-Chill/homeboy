use crate::core::rig::WorkloadSpec;

#[test]
fn test_trace_public_preview_parse() {
    let workload: WorkloadSpec = serde_json::from_str(
        r#"{
            "path": "/tmp/wallet.trace.mjs",
            "public_preview": {
                "mode": "external",
                "local_origin": "http://127.0.0.1:8080",
                "command": "cloudflared tunnel --url http://127.0.0.1:8080",
                "require_https": true,
                "provider": "cloudflared",
                "startup_timeout_seconds": 5,
                "required_asset_paths": [
                    "/assets/app.js"
                ],
                "asset_fanout": {
                    "asset_paths": [
                        "/assets/app.js?ver=1",
                        "/assets/app.css?ver=1"
                    ],
                    "concurrency": 8,
                    "repeat_count": 3,
                    "expected_body_contains": "homeboy-fanout-ok"
                }
            }
        }"#,
    )
    .expect("parse public preview workload");

    let preview = workload.public_preview().expect("public preview");
    assert_eq!(preview.local_origin, "http://127.0.0.1:8080");
    assert_eq!(preview.provider.as_deref(), Some("cloudflared"));
    assert!(preview.require_https);
    assert_eq!(preview.startup_timeout_seconds, Some(5));
    assert_eq!(
        preview.required_asset_paths,
        vec!["/assets/app.js".to_string()]
    );
    let fanout = preview.asset_fanout.as_ref().expect("asset fanout");
    assert_eq!(fanout.asset_paths.len(), 2);
    assert_eq!(fanout.concurrency, Some(8));
    assert_eq!(fanout.repeat_count, Some(3));
    assert_eq!(
        fanout.expected_body_contains.as_deref(),
        Some("homeboy-fanout-ok")
    );
}

#[test]
fn test_trace_public_preview_parse_homeboy_native() {
    let workload: WorkloadSpec = serde_json::from_str(
        r#"{
            "path": "/tmp/wallet.trace.mjs",
            "public_preview": {
                "mode": "homeboy_native",
                "local_origin": "http://127.0.0.1:49823",
                "require_https": true,
                "native": {
                    "operator_domain": "chubes.net",
                    "session_id": "wc-stripe-real-wallet",
                    "ingress_url": "https://preview-broker.chubes.net",
                    "token_env": "HOMEBOY_PREVIEW_TUNNEL_TOKEN"
                }
            }
        }"#,
    )
    .expect("parse native public preview workload");

    let preview = workload.public_preview().expect("public preview");
    assert_eq!(preview.local_origin, "http://127.0.0.1:49823");
    assert_eq!(
        preview.mode,
        crate::core::rig::TracePublicPreviewMode::HomeboyNative
    );
    let native = preview.native.as_ref().expect("native settings");
    assert_eq!(native.operator_domain.as_deref(), Some("chubes.net"));
    assert_eq!(native.session_id.as_deref(), Some("wc-stripe-real-wallet"));
    assert_eq!(
        native.ingress_url.as_deref(),
        Some("https://preview-broker.chubes.net")
    );
    assert_eq!(
        native.token_env.as_deref(),
        Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN")
    );
}
