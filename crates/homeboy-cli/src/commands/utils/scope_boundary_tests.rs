//! Pins the boundary of the shared [`ScopeArgs`](super::args::ScopeArgs) group.
//!
//! #10312 asked for `Scope` to be promoted to a clap arg group and flattened
//! into eight commands. Three of them (`triage landing`, `build`, `status`)
//! migrated in #10510. The other five did **not**, and each refusal is a real
//! property of that command's argument shape rather than an omission:
//!
//! | Command | Why it is not one scope |
//! |---|---|
//! | `deploy` | Its selectors *combine* (`deploy --project P --component C`) instead of excluding each other. |
//! | `refactor` | `-c/--component` is `Append` and `--components` is delimited — a multi-entity target. Its `component`/`path` arg ids also already collide with the group's. |
//! | `release` | `--project` carries the short `-p`, which the deliberately long-only group cannot supply, and `--path` is a per-component checkout override. |
//! | `cleanup artifacts` | Has no entity selection at all; flattening would add five flags with nothing behind them. |
//! | `review` | Already delegates to `PositionalComponentArgs`; there is no six-way switch to remove. |
//!
//! Prose in a merged PR body does not survive contact with the next cook, so
//! the reasons live here as executable assertions. If one of these commands
//! *should* migrate, these tests are the checklist of what has to change with
//! it — every one of them is a user-facing invocation that would break.
//!
//! Lives in its own file rather than inside `args.rs`'s `mod tests` because
//! `args.rs` is already the largest file in `commands/utils` and the audit
//! flags god files.

use crate::cli_surface::Cli;
use clap::{Command, CommandFactory, Parser};

/// Every selector the shared group owns.
const SHARED_GROUP: [&str; 6] = ["project", "fleet", "component", "rig", "path", "workspace"];

/// The command nodes #10510 migrated onto the shared group.
const MIGRATED: [&[&str]; 3] = [&["triage", "landing"], &["build"], &["status"]];

fn parses(argv: &[&str]) -> bool {
    Cli::try_parse_from(argv).is_ok()
}

fn node<'a>(root: &'a Command, path: &[&str]) -> &'a Command {
    let mut current = root;
    for name in path {
        current = current
            .find_subcommand(name)
            .unwrap_or_else(|| panic!("`{name}` should exist in the command tree"));
    }
    current
}

fn declared_longs(command: &Command) -> Vec<&str> {
    command
        .get_arguments()
        .filter_map(clap::Arg::get_long)
        .collect()
}

fn declared_ids(command: &Command) -> Vec<&str> {
    command
        .get_arguments()
        .map(|arg| arg.get_id().as_str())
        .collect()
}

/// The migrated commands really do carry the whole group, so a regression that
/// silently drops one selector is caught here and not by an operator.
#[test]
fn migrated_commands_carry_the_whole_shared_group() {
    let root = Cli::command();
    for path in MIGRATED {
        let longs = declared_longs(node(&root, path));
        for selector in SHARED_GROUP {
            assert!(
                longs.contains(&selector),
                "`homeboy {}` should declare --{selector}",
                path.join(" ")
            );
        }
    }
}

/// `--fleet` is only ever an entity choice, and entity choice is the shared
/// group's job. Any node that declares `--fleet` without the rest of the group
/// is a hand-rolled selector — exactly the duplication #10312 exists to kill.
///
/// `deploy` is the one deliberate exception and is asserted by name so that
/// migrating it, or adding a second exception, is a reviewed decision rather
/// than a silent one.
#[test]
fn hand_rolled_entity_selection_is_confined_to_deploy() {
    fn collect(command: &Command, path: &mut Vec<String>, out: &mut Vec<String>) {
        let longs = declared_longs(command);
        if longs.contains(&"fleet") && !SHARED_GROUP.iter().all(|selector| longs.contains(selector))
        {
            out.push(path.join(" "));
        }
        for subcommand in command.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            collect(subcommand, path, out);
            path.pop();
        }
    }

    let mut offenders = Vec::new();
    collect(
        &Cli::command(),
        &mut vec!["homeboy".to_string()],
        &mut offenders,
    );
    offenders.sort();

    assert_eq!(
        offenders,
        vec!["homeboy deploy".to_string()],
        "a command declares --fleet outside the shared scope group; either \
         flatten ScopeArgs into it or document why its selectors combine",
    );
}

/// `deploy`'s selectors are additive, not exclusive: `--project P --component C`
/// means "these components, on that project". The shared group makes exactly
/// that combination a parse error, so flattening it here would break a
/// documented recipe (`DEPLOY_RECIPES`, `deploy.rs:17`).
#[test]
fn deploy_combines_selectors_the_shared_group_makes_exclusive() {
    assert!(parses(&[
        "homeboy",
        "deploy",
        "--project",
        "growth",
        "--component",
        "homeboy",
    ]));
    assert!(parses(&["homeboy", "deploy", "-p", "growth", "--shared"]));
    // ...and its `--component` is a multi-value target, not one entity.
    assert!(parses(&["homeboy", "deploy", "-c", "alpha", "-c", "beta"]));
}

/// `refactor` targets many components at once. The group is one entity.
#[test]
fn refactor_targets_many_components_at_once() {
    assert!(parses(&[
        "homeboy",
        "refactor",
        "rename",
        "--from",
        "alpha",
        "--to",
        "beta",
        "--component",
        "one",
        "--component",
        "two",
    ]));
    assert!(parses(&[
        "homeboy",
        "refactor",
        "rename",
        "--from",
        "alpha",
        "--to",
        "beta",
        "--components",
        "one,two",
    ]));
}

/// Beyond the semantics, `refactor` cannot even flatten the group *alongside*
/// its existing args: it already owns the `component` and `path` arg ids
/// through `PositionalComponentArgs`, and clap rejects duplicate ids.
#[test]
fn refactor_already_owns_the_arg_ids_the_shared_group_needs() {
    let root = Cli::command();
    let ids = declared_ids(node(&root, &["refactor"]));
    assert!(ids.contains(&"component"), "refactor declares `component`");
    assert!(ids.contains(&"path"), "refactor declares `path`");
}

/// `release --project` is spelled `-p` too. The shared group is deliberately
/// long-flag-only — `deploy -p`, `deploy -c`, and `refactor -c` already mean
/// different things — so migrating `release` would silently delete a short flag
/// that automation uses.
#[test]
fn release_project_selector_keeps_a_short_flag_the_group_cannot_supply() {
    assert!(parses(&["homeboy", "release", "-p", "growth"]));
    assert!(parses(&["homeboy", "release", "--project", "growth"]));

    let root = Cli::command();
    let landing = node(&root, &["triage", "landing"]);
    let project = landing
        .get_arguments()
        .find(|arg| arg.get_id().as_str() == "project")
        .expect("the shared group declares --project");
    assert!(
        project.get_short().is_none(),
        "ScopeArgs is long-flag-only by construction",
    );
}

/// `cleanup artifacts` selects a *checkout root*, not an entity. There is no
/// project, fleet, rig, or component concept behind it, so flattening the group
/// would advertise five flags with nothing implementing them.
#[test]
fn cleanup_artifacts_has_no_entity_selection_to_migrate() {
    assert!(parses(&[
        "homeboy",
        "cleanup",
        "artifacts",
        "--path",
        "/tmp/checkout",
    ]));
    for selector in ["--project", "--fleet", "--component", "--rig"] {
        assert!(
            !parses(&["homeboy", "cleanup", "artifacts", selector, "x"]),
            "{selector} should not exist on cleanup artifacts",
        );
    }
    assert!(!parses(&["homeboy", "cleanup", "artifacts", "--workspace"]));
}

/// `review` selects one component positionally through the older shared group
/// (`PositionalComponentArgs`). There was never a six-way switch here to remove.
#[test]
fn review_selects_a_component_positionally() {
    assert!(parses(&["homeboy", "review", "--path", "/tmp/checkout"]));
    for selector in ["--fleet", "--rig"] {
        assert!(
            !parses(&["homeboy", "review", selector, "x"]),
            "{selector} should not exist on review",
        );
    }
}
