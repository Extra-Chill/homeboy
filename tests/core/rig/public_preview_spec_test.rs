use crate::core::rig::WorkloadSpec;

#[test]
fn test_trace_public_preview_parse() {
    let workload: WorkloadSpec = serde_json::from_str(
        r#"{
            "path": "/tmp/wallet.trace.mjs",
            "public_preview": {
                "local_origin": "http://127.0.0.1:8080",
                "command": "cloudflared tunnel --url http://127.0.0.1:8080",
                "require_https": true,
                "provider": "cloudflared",
                "startup_timeout_seconds": 5,
                "required_asset_paths": [
                    "/wp-content/plugins/woocommerce-gateway-stripe/build/express-checkout.js?ver=10.8.0"
                ]
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
        vec![
            "/wp-content/plugins/woocommerce-gateway-stripe/build/express-checkout.js?ver=10.8.0"
                .to_string()
        ]
    );
}
