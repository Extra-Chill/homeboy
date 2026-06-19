use super::{run, DeployArgs};
use crate::commands::GlobalArgs;
use crate::core::deploy::parse_bulk_component_ids;

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
fn deploy_head_requires_apply_for_real_deploy() {
    let result = run(
        deploy_args(|args| {
            args.target_id = Some("project-a".to_string());
            args.component_ids = vec!["component-a".to_string()];
            args.head = true;
        }),
        &GlobalArgs {},
    );

    let err = match result {
        Ok(_) => panic!("--head real deploy should require --apply"),
        Err(err) => err,
    };
    assert!(err
        .message
        .contains("Real deploys with --head require explicit --apply"));
}

#[test]
fn deploy_force_requires_apply_for_real_deploy() {
    let result = run(
        deploy_args(|args| {
            args.target_id = Some("project-a".to_string());
            args.component_ids = vec!["component-a".to_string()];
            args.force = true;
        }),
        &GlobalArgs {},
    );

    let err = match result {
        Ok(_) => panic!("--force real deploy should require --apply"),
        Err(err) => err,
    };
    assert!(err
        .message
        .contains("Real deploys with --force require explicit --apply"));
}

#[test]
fn deploy_head_dry_run_does_not_require_apply() {
    let result = run(
        deploy_args(|args| {
            args.target_id = Some("missing-project".to_string());
            args.component_ids = vec!["component-a".to_string()];
            args.head = true;
            args.dry_run = true;
        }),
        &GlobalArgs {},
    );

    let err = match result {
        Ok(_) => panic!("dry-run should pass apply boundary before project lookup"),
        Err(err) => err,
    };
    assert!(!err.message.contains("requires explicit --apply"));
}

fn deploy_args(mut customize: impl FnMut(&mut DeployArgs)) -> DeployArgs {
    let mut args = DeployArgs {
        target_id: None,
        component_ids: Vec::new(),
        project: None,
        component: None,
        json: None,
        all: false,
        outdated: false,
        behind_upstream: false,
        dry_run: false,
        apply: false,
        check: false,
        force: false,
        projects: None,
        fleet: None,
        shared: false,
        keep_deps: false,
        version: None,
        no_pull: false,
        head: false,
        tagged: false,
    };
    customize(&mut args);
    args
}
