use super::{resolve_multi_args, run, DeployArgs};
use crate::cli_surface::{Cli, Commands};
use crate::commands::GlobalArgs;
use clap::Parser;

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

#[test]
fn multi_project_resolves_positional_components() {
    let (components, config) = resolve_multi_args(&deploy_args(|args| {
        args.projects = Some(vec!["project-a".to_string(), "project-b".to_string()]);
        args.target_id = Some("component-a".to_string());
        args.component_ids = vec!["component-b".to_string()];
    }))
    .expect("positional components should resolve");

    assert_eq!(components, ["component-a", "component-b"]);
    assert_eq!(config.component_ids, components);
}

#[test]
fn multi_project_resolves_component_flag_components() {
    let (components, config) = resolve_multi_args(&deploy_args(|args| {
        args.projects = Some(vec!["project-a".to_string(), "project-b".to_string()]);
        args.component = Some(vec!["component-a".to_string(), "component-b".to_string()]);
    }))
    .expect("component flag components should resolve");

    assert_eq!(components, ["component-a", "component-b"]);
    assert_eq!(config.component_ids, components);
}

#[test]
fn multi_project_resolves_json_components() {
    let (components, config) = resolve_multi_args(&deploy_args(|args| {
        args.projects = Some(vec!["project-a".to_string(), "project-b".to_string()]);
        args.json = Some(r#"{"component_ids":["component-a","component-b"]}"#.to_string());
    }))
    .expect("json components should resolve");

    assert_eq!(components, ["component-a", "component-b"]);
    assert_eq!(config.component_ids, components);
}

#[test]
fn multi_project_zero_components_remains_validation_failure() {
    let (components, config) = resolve_multi_args(&deploy_args(|args| {
        args.projects = Some(vec!["project-a".to_string(), "project-b".to_string()]);
    }))
    .expect("empty component input is resolved for core validation");

    assert!(components.is_empty());
    assert!(config.component_ids.is_empty());

    let err = homeboy::core::deploy::run_multi(
        &["project-a".to_string(), "project-b".to_string()],
        &components,
        &config,
    )
    .expect_err("zero components should fail multi-project validation");

    assert_eq!(err.details["field"], "component_ids");
    assert!(err
        .message
        .contains("At least one component ID is required for multi-project deployment"));
}

#[test]
fn deploy_parser_keeps_positionals_as_components_with_explicit_projects() {
    let cli = Cli::parse_from([
        "homeboy",
        "deploy",
        "--projects",
        "project-a,project-b",
        "component-a",
        "component-b",
    ]);

    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };

    assert_eq!(
        args.projects,
        Some(vec!["project-a".to_string(), "project-b".to_string()])
    );
    assert_eq!(args.target_id, Some("component-a".to_string()));
    assert_eq!(args.component_ids, ["component-b"]);
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
