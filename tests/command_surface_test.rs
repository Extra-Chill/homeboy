use homeboy::cli_surface::current_command_surface;
use std::collections::BTreeSet;

#[test]
fn includes_current_top_level_commands() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["audit"]));
    assert!(surface.contains_path(&["daemon"]));
    assert!(surface.contains_path(&["deps"]));
    assert!(surface.contains_path(&["git"]));
    assert!(surface.contains_path(&["self"]));
    assert!(surface.contains_path(&["stack"]));
    assert!(surface.contains_path(&["report"]));
    assert!(!surface.contains_path(&["transfer"]));
}

#[test]
fn includes_first_level_subcommands() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["git", "status"]));
    assert!(surface.contains_path(&["deps", "status"]));
    assert!(surface.contains_path(&["deps", "update"]));
    assert!(surface.contains_path(&["daemon", "serve"]));
    assert!(surface.contains_path(&["self", "status"]));
    assert!(surface.contains_path(&["stack", "inspect"]));
    assert!(surface.contains_path(&["report", "failure-digest"]));
    assert!(surface.contains_path(&["file", "download"]));
    assert!(surface.contains_path(&["file", "upload"]));
    assert!(surface.contains_path(&["file", "copy"]));
    assert!(surface.contains_path(&["file", "sync"]));
}

#[test]
fn includes_visible_aliases() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["components"]));
    assert!(surface.contains_path(&["dependencies"]));
    assert!(surface.contains_path(&["rigs"]));
    assert!(surface.contains_path(&["stacks", "inspect"]));
}

#[test]
fn rejects_stale_or_deeper_paths() {
    let surface = current_command_surface();

    assert!(!surface.contains_path(&["supports"]));
    assert!(!surface.contains_path(&["audit", "code"]));
    assert!(!surface.contains_path(&["stack", "inspect", "extra"]));
}

#[test]
fn command_index_matches_top_level_command_surface() {
    let surface = current_command_surface();
    let documented = documented_command_index_entries();

    let extension_commands = BTreeSet::from(["cargo".to_string(), "wp".to_string()]);
    let expected: BTreeSet<String> = surface
        .commands
        .iter()
        .map(|entry| entry.name.clone())
        .chain(extension_commands.iter().cloned())
        .collect();

    let missing: Vec<_> = expected.difference(&documented).cloned().collect();
    let stale: Vec<_> = documented.difference(&expected).cloned().collect();

    assert!(
        missing.is_empty(),
        "docs/commands/commands-index.md is missing top-level commands: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "docs/commands/commands-index.md lists stale top-level commands: {stale:?}"
    );
}

fn documented_command_index_entries() -> BTreeSet<String> {
    let index = include_str!("../docs/commands/commands-index.md");
    let command_section = index.split("Related:").next().unwrap_or(index);

    command_section
        .lines()
        .filter_map(|line| line.strip_prefix("- ["))
        .filter_map(|rest| rest.split(']').next())
        .map(str::to_string)
        .collect()
}
