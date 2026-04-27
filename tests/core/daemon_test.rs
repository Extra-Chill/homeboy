use super::*;
use crate::test_support::HomeGuard;

#[test]
fn parse_bind_addr_defaults_to_loopback_shape() {
    let addr = parse_bind_addr(DEFAULT_ADDR).expect("parse default");

    assert!(addr.ip().is_loopback());
    assert_eq!(addr.port(), 0);
}

#[test]
fn parse_bind_addr_rejects_public_bind() {
    let err = parse_bind_addr("0.0.0.0:8080").expect_err("reject public bind");

    assert!(err.message.contains("loopback"));
}

#[test]
fn routes_health_version_and_config_paths() {
    let _home = HomeGuard::new();

    let health = route("GET", "/health");
    assert_eq!(health.status_code, 200);
    assert_eq!(health.body["status"], "ok");

    let version = route("GET", "/version");
    assert_eq!(version.status_code, 200);
    assert_eq!(version.body["version"], env!("CARGO_PKG_VERSION"));

    let paths = route("GET", "/config/paths");
    assert_eq!(paths.status_code, 200);
    assert!(paths.body["homeboy"]
        .as_str()
        .unwrap()
        .ends_with(".config/homeboy"));
    assert!(paths.body["daemon_state"]
        .as_str()
        .unwrap()
        .ends_with("daemon/state.json"));
}

#[test]
fn route_rejects_unknown_paths_and_methods() {
    assert_eq!(route("GET", "/missing").status_code, 404);
    assert_eq!(route("POST", "/health").status_code, 405);
}

#[test]
fn status_is_not_running_without_state_file() {
    let _home = HomeGuard::new();

    let status = read_status().expect("status");
    assert!(!status.running);
    assert!(status.state.is_none());
    assert!(status.state_path.ends_with("daemon/state.json"));
}
