//! Coverage guards for the generated CLI reference, plus its regenerator.
//!
//! Lives beside `cli_surface/mod.rs` rather than inside its `mod tests` block
//! for the same reason as `global_flag_surface_tests`: that file sits a handful
//! of lines under the audit's 1500-line `god_file` threshold.
//!
//! The generated tree under `docs/reference/cli/` is **not** gated in CI. Byte
//! currency between the checked-in pages, the serialized contract, and the live
//! clap tree used to be enforced by `homeboy / CLI Reference Docs` and
//! `homeboy / CLI Reference Runtime Parity`; both were removed deliberately
//! because a docs regeneration step is not worth blocking merges over. Refresh
//! the tree on demand with:
//!
//! ```text
//! HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
//! ```

use super::reference_docs::{
    commands_without_description, documented_subcommands, generated_reference_docs,
    live_generated_reference_docs, GENERATED_DIR, WRITE_ENV,
};
use super::Cli;
use clap::CommandFactory;
use homeboy_command_contract::cli_reference::CliReference;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Repo-root-relative paths resolve from the workspace root, not this crate's
/// manifest dir.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Regenerates the checked-in CLI reference tree from the live clap command
/// tree. This is a generator entry point, not a gate: without
/// `HOMEBOY_WRITE_CLI_REFERENCE` set it does nothing and asserts nothing.
///
/// It stays shaped as a `#[test]` because that is the only way to reach the
/// clap tree with the workspace already built, and it keeps the documented
/// regeneration command working.
#[test]
fn cli_reference_docs_regenerate_on_demand() {
    if std::env::var_os(WRITE_ENV).is_none() {
        return;
    }

    let directory = workspace_root().join(GENERATED_DIR);
    let expected = live_generated_reference_docs();
    if directory.exists() {
        std::fs::remove_dir_all(&directory).expect("failed to clear the generated CLI reference");
    }
    std::fs::create_dir_all(&directory)
        .expect("failed to create the generated CLI reference directory");
    for (name, body) in &expected {
        std::fs::write(directory.join(name), body)
            .unwrap_or_else(|error| panic!("failed to write {GENERATED_DIR}/{name}: {error}"));
    }
    let contract = serde_json::to_string_pretty(&CliReference::new(expected))
        .expect("serialize CLI reference contract");
    std::fs::write(
        workspace_root().join("docs/reference/cli/command-surface.json"),
        format!("{contract}\n"),
    )
    .expect("write CLI reference contract");
}

/// One generated page per visible top-level command, plus the index.
#[test]
fn generated_reference_covers_every_visible_top_level_command() {
    let root = Cli::command();

    let expected = documented_subcommands(&root)
        .into_iter()
        .map(|command| format!("{}.md", command.get_name()))
        .chain(std::iter::once("index.md".to_string()))
        .collect::<BTreeSet<_>>();

    assert_eq!(
        generated_reference_docs()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        expected
    );
}

/// A command with no clap `about` is invisible in `--help`, in the generated
/// reference, and to any agent reading the command surface.
///
/// `agent-task` used to be the one family shipping subcommands without help
/// text, so this guard could only pin the *shape* of that debt (#10324). All 40
/// of those variants now carry doc comments (#11147), so the guard is exact: any
/// new visible command node without help text fails here.
#[test]
fn every_visible_command_ships_help_text() {
    let root = Cli::command();

    let mut undocumented = Vec::new();
    for command in documented_subcommands(&root) {
        commands_without_description(
            &[command.get_name().to_string()],
            command,
            &mut undocumented,
        );
    }

    assert!(
        undocumented.is_empty(),
        "these commands ship without clap help text: {undocumented:?}. Add a doc \
         comment to the clap variant instead of widening this guard."
    );
}

/// `documented_command_index_entries` collects into a `BTreeSet` and the
/// registry guard uses `index.contains(..)`, so a duplicated index line is
/// invisible to both `self doctor` and
/// `command_registry_docs_paths_exist_and_are_indexed`. It shipped: `api` was
/// listed twice (#10324).
#[test]
fn commands_index_lists_each_command_once() {
    let index = std::fs::read_to_string(workspace_root().join("docs/commands/commands-index.md"))
        .expect("failed to read docs/commands/commands-index.md");

    // Mirror `documented_command_index_entries`: only the command section above
    // `Related:` is the command list.
    let command_section = index.split("Related:").next().unwrap_or(index.as_str());

    let mut seen = BTreeSet::new();
    let mut duplicates = Vec::new();
    for line in command_section.lines() {
        let Some(rest) = line.strip_prefix("- [") else {
            continue;
        };
        let Some(slug) = rest.split(']').next() else {
            continue;
        };
        if !seen.insert(slug.to_string()) {
            duplicates.push(slug.to_string());
        }
    }

    assert!(
        duplicates.is_empty(),
        "docs/commands/commands-index.md lists these commands more than once: {duplicates:?}"
    );
}
