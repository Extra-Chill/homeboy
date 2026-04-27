use homeboy::deploy::parse_bulk_component_ids;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-deploy-{name}-{nanos}"))
}

#[test]
fn test_parse_bulk_component_ids_supports_json_array() {
    let ids = parse_bulk_component_ids(r#"["api","web"]"#).unwrap();
    assert_eq!(ids, vec!["api", "web"]);
}

#[test]
fn test_parse_bulk_component_ids_supports_json_object() {
    let ids = parse_bulk_component_ids(r#"{"component_ids":["api","web"]}"#).unwrap();
    assert_eq!(ids, vec!["api", "web"]);
}

#[test]
fn test_parse_bulk_component_ids_rejects_csv() {
    assert!(parse_bulk_component_ids("api, web").is_err());
}
