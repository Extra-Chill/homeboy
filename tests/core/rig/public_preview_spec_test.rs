use crate::{TracePublicPreviewMode, WorkloadSpec};

/// Rig packages author `public_preview` in workload JSON; both preview modes
/// and their nested settings must keep parsing from that wire shape.
#[test]
fn test_trace_public_preview_parse() {
    let external: WorkloadSpec = serde_json::from_str(
        r#"{
            "path": "/tmp/wallet.trace.mjs",
            "public_preview": {
                "mode": "external",
                "local_origin": "http://127.0.0.1:8080",
                "command": "cloudflared tunnel --url http://127.0.0.1:8080",
                "require_https": true,
                "provider": "cloudflared",
                "startup_timeout_seconds": 5,
                "required_asset_paths": ["/assets/app.js"],
                "asset_fanout": {
                    "asset_paths": ["/assets/app.js?ver=1", "/assets/app.css?ver=1"],
                    "concurrency": 8,
                    "repeat_count": 3,
                    "expected_body_contains": "homeboy-fanout-ok"
                }
            }
        }"#,
    )
    .expect("parse public preview workload");

    let preview = external.public_preview().expect("public preview");
    assert_eq!(preview.mode, TracePublicPreviewMode::External);
    assert!(preview.require_https);
    assert_eq!(preview.required_asset_paths, vec!["/assets/app.js"]);
    let fanout = preview.asset_fanout.as_ref().expect("asset fanout");
    assert_eq!(fanout.asset_paths.len(), 2);
    assert_eq!(fanout.concurrency, Some(8));
    assert_eq!(
        fanout.expected_body_contains.as_deref(),
        Some("homeboy-fanout-ok")
    );

    let native: WorkloadSpec = serde_json::from_str(
        r#"{
            "path": "/tmp/wallet.trace.mjs",
            "public_preview": {
                "mode": "homeboy_native",
                "local_origin": "http://127.0.0.1:49823",
                "native": {
                    "operator_domain": "preview.example.test",
                    "session_id": "wc-stripe-real-wallet",
                    "ingress_url": "https://preview-broker.example.test",
                    "token_env": "HOMEBOY_PREVIEW_TUNNEL_TOKEN"
                }
            }
        }"#,
    )
    .expect("parse native public preview workload");

    let preview = native.public_preview().expect("public preview");
    assert_eq!(preview.mode, TracePublicPreviewMode::HomeboyNative);
    let native = preview.native.as_ref().expect("native settings");
    assert_eq!(
        native.ingress_url.as_deref(),
        Some("https://preview-broker.example.test")
    );
    assert_eq!(
        native.token_env.as_deref(),
        Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN")
    );
}
