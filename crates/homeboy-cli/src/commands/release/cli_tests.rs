use super::{ReleaseReadinessCommand, ReleaseSubcommand, ResolveOutput};
use crate::cli_surface::{Cli, Commands};
use clap::{CommandFactory, Parser};

fn release(argv: &[&str]) -> super::ReleaseArgs {
    let cli = Cli::try_parse_from(argv)
        .unwrap_or_else(|error| panic!("release invocation failed to parse: {argv:?}\n{error}"));
    let Commands::Release(args) = cli.command else {
        panic!("expected release command");
    };
    args
}

#[test]
fn release_envelope_flags_compose_on_both_sides_of_inspection_subcommands() {
    for argv in [
        ["homeboy", "release", "--full", "changes", "homeboy"].as_slice(),
        [
            "homeboy",
            "release",
            "changes",
            "data-machine-code",
            "--path",
            "/path/to/repo",
            "--full",
        ]
        .as_slice(),
    ] {
        assert!(release(argv).execute.full);
    }

    for argv in [
        [
            "homeboy",
            "release",
            "--output",
            "./release.json",
            "gap",
            "homeboy",
        ]
        .as_slice(),
        [
            "homeboy",
            "release",
            "gap",
            "homeboy",
            "--output",
            "./release.json",
        ]
        .as_slice(),
        [
            "homeboy",
            "release",
            "--notification-transport",
            "slack",
            "--notification-route",
            "release-room",
            "readiness",
            "list",
            "homeboy",
        ]
        .as_slice(),
        [
            "homeboy",
            "release",
            "readiness",
            "list",
            "homeboy",
            "--notification-transport",
            "slack",
            "--notification-route",
            "release-room",
        ]
        .as_slice(),
    ] {
        release(argv);
    }
}

#[test]
fn release_inspection_targets_use_component_first_grammar() {
    let changes = release(&["homeboy", "release", "changes", "homeboy"]);
    assert!(matches!(
        changes.command,
        Some(ReleaseSubcommand::Changes(_))
    ));

    let contains = release(&["homeboy", "release", "contains", "homeboy", "6043c013d"]);
    let Some(ReleaseSubcommand::Contains(contains)) = contains.command else {
        panic!("expected release contains");
    };
    assert_eq!(contains.target.as_deref(), Some("homeboy"));
    assert_eq!(contains.commit.as_deref(), Some("6043c013d"));

    let gap = release(&["homeboy", "release", "gap", "homeboy"]);
    let Some(ReleaseSubcommand::Gap(gap)) = gap.command else {
        panic!("expected release gap");
    };
    assert_eq!(gap.component_id.as_deref(), Some("homeboy"));

    let readiness = release(&["homeboy", "release", "readiness", "list", "homeboy"]);
    let Some(ReleaseSubcommand::Readiness(readiness)) = readiness.command else {
        panic!("expected release readiness");
    };
    assert!(matches!(
        readiness.command,
        ReleaseReadinessCommand::List { component_id } if component_id == "homeboy"
    ));

    let resolve = release(&[
        "homeboy",
        "release",
        "resolve",
        "https://github.com/example/monorepo.git",
        "--prefix",
        "figma-transformer",
    ]);
    assert!(matches!(
        resolve.command,
        Some(ReleaseSubcommand::Resolve(_))
    ));
}

#[test]
fn changelog_history_repair_requires_recovery_and_parses_apply() {
    let args = release(&[
        "homeboy",
        "release",
        "fixture",
        "--recover",
        "--repair-changelog-history",
        "1.0.0,1.1.0",
        "--repair-changelog-history",
        "1.2.0",
        "--apply",
    ]);
    assert!(args.execute.recover);
    assert_eq!(
        args.execute.repair_changelog_history,
        ["1.0.0", "1.1.0", "1.2.0"]
    );
    assert!(args.execute.apply);

    let missing_recover = Cli::try_parse_from([
        "homeboy",
        "release",
        "fixture",
        "--repair-changelog-history",
        "1.0.0",
    ]);
    assert!(missing_recover.is_err());

    let missing_versions = Cli::try_parse_from([
        "homeboy",
        "release",
        "fixture",
        "--recover",
        "--repair-changelog-history",
    ]);
    assert!(missing_versions.is_err());
}

#[test]
fn release_inspection_help_includes_copy_paste_examples() {
    let expectations = [
        (
            ["release", "changes"].as_slice(),
            "homeboy release changes data-machine-code --path /path/to/repo --full",
        ),
        (
            ["release", "contains"].as_slice(),
            "homeboy release contains data-machine-code 6043c013d",
        ),
        (
            ["release", "gap"].as_slice(),
            "homeboy release gap data-machine-code",
        ),
        (
            ["release", "readiness"].as_slice(),
            "homeboy release readiness list data-machine-code",
        ),
        (
            ["release", "resolve"].as_slice(),
            "homeboy release resolve https://github.com/example/monorepo.git --prefix figma-transformer",
        ),
    ];

    for (path, example) in expectations {
        let mut command = Cli::command();
        for segment in path {
            command = command
                .find_subcommand(segment)
                .unwrap_or_else(|| panic!("missing command path segment `{segment}`"))
                .clone();
        }
        let help = command.render_help().to_string();
        assert!(help.contains(example), "missing `{example}`:\n{help}");
    }
}

#[test]
fn resolve_output_keeps_success_and_no_match_shapes_distinct() {
    let success = serde_json::to_value(ResolveOutput {
        command: "release.resolve",
        variant: "resolve",
        repository: "file:///repo.git".to_string(),
        version: Some("1.2.3".to_string()),
        tag: Some("component-v1.2.3".to_string()),
        commit: Some("a".repeat(40)),
        no_match: None,
    })
    .unwrap();
    assert_eq!(success["command"], "release.resolve");
    assert!(success.get("match").is_none());

    let no_match = serde_json::to_value(ResolveOutput {
        command: "release.resolve",
        variant: "resolve",
        repository: "file:///repo.git".to_string(),
        version: None,
        tag: None,
        commit: None,
        no_match: Some(serde_json::Value::Null),
    })
    .unwrap();
    assert_eq!(no_match["match"], serde_json::Value::Null);
    assert!(no_match.get("version").is_none());
}
