use super::{run, DeployArgs};
use crate::cli_surface::{Cli, Commands};
use clap::Parser;
use std::collections::BTreeMap;

/// Every dangerous source mode refuses a real deploy without
/// `--confirm-dangerous`, and names itself in the refusal.
#[test]
fn dangerous_real_deploys_require_explicit_confirmation() {
    let manifest = tempfile::NamedTempFile::new().expect("manifest file");
    std::fs::write(
        manifest.path(),
        r#"{"schema":"homeboy/release-set/v1","components":[{"id":"fixture","ref":"accepted"}]}"#,
    )
    .expect("manifest");
    let manifest_path = manifest.path().display().to_string();

    let cases: [(&str, Box<dyn Fn(&mut DeployArgs)>); 4] = [
        ("--head", Box::new(|args| args.head = true)),
        ("--force", Box::new(|args| args.force = true)),
        (
            "--ref",
            Box::new(|args| args.requested_ref = Some("accepted-commit".to_string())),
        ),
        (
            "--release-set",
            Box::new(move |args| args.release_set = Some(manifest_path.clone())),
        ),
    ];
    for (flag, customize) in cases {
        let error = match run(deploy_args(|args| {
            args.target_id = Some("project-a".to_string());
            args.component_ids = vec!["component-a".to_string()];
            customize(args);
        })) {
            Ok(_) => panic!("{flag} real deploy should require --confirm-dangerous"),
            Err(error) => error,
        };
        assert!(
            error
                .message
                .contains(&format!("{flag} require explicit --confirm-dangerous")),
            "{flag}: {}",
            error.message
        );
    }
}

#[test]
fn deploy_head_dry_run_does_not_require_dangerous_confirmation() {
    let result = run(deploy_args(|args| {
        args.target_id = Some("missing-project".to_string());
        args.component_ids = vec!["component-a".to_string()];
        args.head = true;
        args.dry_run = true;
    }));

    let err = match result {
        Ok(_) => panic!(
            "dry-run should pass the dangerous-mode confirmation boundary before project lookup"
        ),
        Err(err) => err,
    };
    assert!(!err
        .message
        .contains("requires explicit --confirm-dangerous"));
}

#[test]
fn release_set_check_is_rejected_before_ref_resolution_or_materialization() {
    let result = run(deploy_args(|args| {
        args.release_set = Some("not-read.json".to_string());
        args.check = true;
    }));
    let error = match result {
        Ok(_) => panic!(
            "release-set check must be rejected before it reads or mutates a source checkout"
        ),
        Err(error) => error,
    };

    assert!(error
        .message
        .contains("--check cannot be combined with --release-set"));
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
        confirm_dangerous: false,
        check: false,
        verify: false,
        force: false,
        keep_deps: false,
        version: None,
        no_pull: false,
        allow_stale_source: false,
        allow_downgrade: false,
        head: false,
        release_set: None,
        requested_ref: None,
        tagged: false,
        target: None,
        exact_refs: BTreeMap::new(),
        resolved_refs: BTreeMap::new(),
        preflighted_source_paths: BTreeMap::new(),
        preflighted_component_identities: BTreeMap::new(),
    };
    customize(&mut args);
    args
}

/// `--ref` selects immutable source; `--version` asserts what that source
/// builds. Recovery needs both at once (#10648).
#[test]
fn exact_ref_deploy_accepts_an_independent_expected_version() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "deploy",
        "--project",
        "project-a",
        "--component",
        "component-a",
        "--ref",
        "v0.170.8",
        "--version",
        "0.170.8",
    ])
    .expect("--ref and --version answer different questions and must combine");

    let Commands::Deploy(args) = cli.command else {
        panic!("expected deploy command");
    };
    assert_eq!(args.requested_ref.as_deref(), Some("v0.170.8"));
    assert_eq!(args.version.as_deref(), Some("0.170.8"));
}
