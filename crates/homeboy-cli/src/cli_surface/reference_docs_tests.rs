//! Currency and coverage guards for the generated CLI reference.
//!
//! Lives beside `cli_surface/mod.rs` rather than inside its `mod tests` block
//! for the same reason as `global_flag_surface_tests`: that file sits a handful
//! of lines under the audit's 1500-line `god_file` threshold.
//!
//! Every `#[derive(Subcommand)]` and `#[derive(Args)]` in the workspace lives in
//! `crates/homeboy-cli`, so any change that can move the command surface must
//! touch this crate. That makes the differential `review test` gate a real
//! guard for these tests, and `.github/workflows/cli-reference-docs.yml` runs
//! the same generation non-differentially (and uploads the regenerated tree) on
//! every PR that touches the CLI crate.

use super::reference_docs::{
    commands_without_description, documented_subcommands, generated_reference_docs,
    live_generated_reference_docs, GENERATED_DIR, WRITE_ENV,
};
use super::Cli;
use clap::CommandFactory;
use homeboy_command_contract::cli_reference::CliReference;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Repo-root-relative paths resolve from the workspace root, not this crate's
/// manifest dir.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn checked_in_reference_docs() -> BTreeMap<String, String> {
    let directory = workspace_root().join(GENERATED_DIR);
    let mut docs = BTreeMap::new();

    let Ok(entries) = std::fs::read_dir(&directory) else {
        return docs;
    };

    for entry in entries {
        let path = entry
            .expect("failed to read a generated CLI reference entry")
            .path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("generated CLI reference filenames should be valid UTF-8")
            .to_string();
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {GENERATED_DIR}/{name}: {error}"));
        docs.insert(name, body);
    }

    docs
}

/// The generated reference must byte-match the live clap command tree.
///
/// With `HOMEBOY_WRITE_CLI_REFERENCE` set, this rewrites the tree instead of
/// asserting, which is how the tree is regenerated and how CI produces the
/// downloadable artifact for a stale PR.
#[test]
fn cli_reference_docs_are_current() {
    let directory = workspace_root().join(GENERATED_DIR);

    if std::env::var_os(WRITE_ENV).is_some() {
        let expected = live_generated_reference_docs();
        if directory.exists() {
            std::fs::remove_dir_all(&directory)
                .expect("failed to clear the generated CLI reference");
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
        return;
    }

    let expected = generated_reference_docs();

    let actual = checked_in_reference_docs();
    let regenerate = format!(
        "regenerate with `{WRITE_ENV}=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs`"
    );

    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "{GENERATED_DIR} does not contain exactly one page per visible top-level command; {regenerate}"
    );

    for (name, body) in &expected {
        let current = actual
            .get(name)
            .map(String::as_str)
            .expect("file set already asserted equal");
        assert!(
            current == body,
            "{GENERATED_DIR}/{name} is stale ({} checked-in lines vs {} generated lines); {regenerate}",
            current.lines().count(),
            body.lines().count(),
        );
    }
}

#[test]
fn live_clap_reference_matches_serialized_contract() {
    assert_eq!(live_generated_reference_docs(), generated_reference_docs());
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
/// `agent-task` is the one family that still ships subcommands without help
/// text (#10324). Pinning the *shape* of that debt rather than a count keeps
/// the guard honest: no other family may acquire the same gap, and the exact
/// remaining paths are listed in the generated index, so shrinking the set
/// shows up as a reviewed docs diff.
#[test]
fn commands_without_help_text_are_confined_to_agent_task() {
    let root = Cli::command();

    let mut undocumented = Vec::new();
    for command in documented_subcommands(&root) {
        commands_without_description(
            &[command.get_name().to_string()],
            command,
            &mut undocumented,
        );
    }

    let stray = undocumented
        .iter()
        .filter(|path| !path.starts_with("agent-task"))
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        stray.is_empty(),
        "these commands ship without clap help text: {stray:?}. Add a doc comment to \
         the clap variant instead of widening this guard."
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
