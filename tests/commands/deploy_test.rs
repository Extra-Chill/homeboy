use super::{resolve_multi_args, run, DeployArgs};
use crate::cli_surface::{Cli, Commands};
use crate::commands::GlobalArgs;
use clap::Parser;
use std::collections::BTreeMap;

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
fn deploy_ref_requires_apply_for_real_deploy() {
    let result = run(
        deploy_args(|args| {
            args.target_id = Some("project-a".to_string());
            args.component_ids = vec!["component-a".to_string()];
            args.requested_ref = Some("accepted-commit".to_string());
        }),
        &GlobalArgs {},
    );

    let err = match result {
        Ok(_) => panic!("--ref real deploy should require --apply"),
        Err(err) => err,
    };
    assert!(err
        .message
        .contains("Real deploys with --ref require explicit --apply"));
}

#[test]
fn deploy_parser_accepts_exact_ref() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "deploy",
        "project-a",
        "component-a",
        "--ref",
        "release-candidate",
        "--dry-run",
    ])
    .expect("--ref should parse");

    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };
    assert_eq!(args.requested_ref.as_deref(), Some("release-candidate"));
}

#[test]
fn deploy_parser_accepts_release_set_manifest() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "deploy",
        "--project",
        "project-a",
        "--release-set",
        "release-set.json",
        "--dry-run",
    ])
    .expect("--release-set should parse");

    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };
    assert_eq!(args.release_set.as_deref(), Some("release-set.json"));
}

#[test]
fn release_set_rejects_conflicting_source_selectors() {
    for conflicting in [vec!["--head"], vec!["--tagged"], vec!["--outdated"]] {
        let mut argv = vec![
            "homeboy",
            "deploy",
            "--project",
            "project-a",
            "--release-set",
            "release-set.json",
        ];
        argv.extend(conflicting.iter().copied());
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "--release-set should conflict with {conflicting:?}"
        );
    }
}

#[test]
fn release_set_rejects_multi_target_modes() {
    for target in [vec!["--projects", "project-a,project-b"], vec!["--fleet", "fleet-a"], vec!["--shared"]] {
        let mut argv = vec!["homeboy", "deploy", "--release-set", "release-set.json"];
        argv.extend(target.iter().copied());
        let cli = Cli::try_parse_from(argv).expect("multi-target selector should parse for diagnostic");
        let Commands::Deploy(args) = cli.command else {
            panic!("expected deploy command");
        };
        let error = match run(args, &GlobalArgs {}) {
            Ok(_) => panic!("release-set multi-target deploy must be rejected"),
            Err(error) => error,
        };
        assert!(error.message.contains("one --project deployment at a time"));
    }
}

#[test]
fn release_set_requires_apply_before_preflight() {
    let manifest = tempfile::NamedTempFile::new().expect("manifest file");
    std::fs::write(
        manifest.path(),
        r#"{"schema":"homeboy/release-set/v1","components":[{"id":"fixture","ref":"accepted"}]}"#,
    )
    .expect("manifest");
    let args = deploy_args(|args| {
        args.release_set = Some(manifest.path().display().to_string());
    });

    let error = match run(args, &GlobalArgs {}) {
        Ok(_) => panic!("release set must require --apply"),
        Err(error) => error,
    };
    assert!(error.message.contains("--release-set require explicit --apply"));
}

#[test]
fn release_set_check_is_rejected_before_ref_resolution_or_materialization() {
    let result = run(
        deploy_args(|args| {
            args.release_set = Some("not-read.json".to_string());
            args.check = true;
        }),
        &GlobalArgs {},
    );
    let error = match result {
        Ok(_) => panic!("release-set check must be rejected before it reads or mutates a source checkout"),
        Err(error) => error,
    };

    assert!(error.message.contains("--check cannot be combined with --release-set"));
}

#[test]
fn deploy_resume_run_id_propagates_to_multi_target_config() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "deploy",
        "component-a",
        "--projects",
        "project-a,project-b",
        "--resume",
        "run-123",
    ])
    .expect("--resume should parse");

    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };
    let (_, config) = resolve_multi_args(&args).expect("deploy config should resolve");

    assert_eq!(config.resume_run_id.as_deref(), Some("run-123"));
}

#[test]
fn skip_deps_hydration_cli_flag_propagates_to_deploy_config() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "--skip-deps-hydration",
        "deploy",
        "project-a",
        "component-a",
    ])
    .expect("--skip-deps-hydration should parse");

    crate::commands::set_skip_deps_hydration(cli.skip_deps_hydration);
    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };
    let (_, config) = resolve_multi_args(&args).expect("deploy config should resolve");
    crate::commands::set_skip_deps_hydration(false);

    assert!(config.skip_deps_hydration);
}

#[test]
fn deploy_apply_does_not_grant_stale_or_downgrade_consent() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "deploy",
        "project-a",
        "component-a",
        "--apply",
    ])
    .expect("--apply should parse");

    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };
    assert!(args.apply);
    assert!(!args.allow_stale_source);
    assert!(!args.allow_downgrade);
}

#[test]
fn deploy_parser_accepts_explicit_source_safety_overrides() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "deploy",
        "project-a",
        "component-a",
        "--allow-stale-source",
        "--allow-downgrade",
    ])
    .expect("source-safety overrides should parse");

    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };
    assert!(args.allow_stale_source);
    assert!(args.allow_downgrade);
}

#[test]
fn deploy_ref_rejects_every_other_source_selector() {
    for conflicting in [
        vec!["--head"],
        vec!["--tagged"],
        vec!["--version", "1.2.3"],
        vec!["--outdated"],
        vec!["--behind-upstream"],
        vec!["--check"],
    ] {
        let mut argv = vec![
            "homeboy",
            "deploy",
            "project-a",
            "component-a",
            "--ref",
            "accepted-commit",
        ];
        argv.extend(conflicting.iter().copied());
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "--ref should conflict with {conflicting:?}"
        );
    }
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

    let err = homeboy_release::deploy::run_multi(
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
        allow_stale_source: false,
        allow_downgrade: false,
        head: false,
        release_set: None,
        requested_ref: None,
        tagged: false,
        resume: None,
        exact_refs: BTreeMap::new(),
        resolved_refs: BTreeMap::new(),
        preflighted_source_paths: BTreeMap::new(),
        preflighted_component_identities: BTreeMap::new(),
    };
    customize(&mut args);
    args
}
