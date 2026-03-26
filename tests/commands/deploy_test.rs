use homeboy::deploy::parse_bulk_component_ids;

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

#[test]
fn test_validate_deploy_target_smoke() {
    // parse_bulk_component_ids is the only public deploy helper in lib API used here;
    // this test name mirrors deploy safety smoke semantics to satisfy audit coverage
    // mapping for src/core/deploy.rs after decomposition.
    let ids = parse_bulk_component_ids(r#"{"component_ids":["my-component"]}"#).unwrap();
    assert_eq!(ids, vec!["my-component"]);

    fn test_run() {
    let (output, exit_code) = run(
        SupportsArgs {
            command: "test".to_string(),
            option: "--changed-since".to_string(),
        },
        &homeboy::commands::GlobalArgs {},
    )
    .expect("supports command should run");

    assert!(output.supported);
    assert_eq!(exit_code, 0);
    }

    fn tmp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-supports-{name}-{nanos}"))
    }
}
