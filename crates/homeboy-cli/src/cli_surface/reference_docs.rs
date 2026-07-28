//! Generates the CLI reference under `docs/reference/cli/commands/` from the
//! clap command tree.
//!
//! # Why reflective, and not `--help` scraping
//!
//! Rendered `--help` is a *wire protocol* in this repository:
//! `homeboy-lab-runner`'s `negotiate_leaseless_recovery_contract` parses a
//! remote binary's rendered `Options:` block to negotiate a capability
//! handshake. Generating docs by shelling out to `--help` would couple doc
//! generation to that rendering, and would also require a built binary. Walking
//! `clap::Command` reflectively reads the same source of truth without touching
//! the rendered surface at all: this module only *reads* `Command`/`Arg`
//! accessors, so it can never change what `--help` prints.
//!
//! # Why checked in, and not generated at build time
//!
//! `crates/homeboy-cli/build.rs` already embeds every `docs/**/*.md` file into
//! the binary and emits a `rerun-if-changed` for each one. Generating this tree
//! during the build would add a second pass over the whole command surface to
//! every single compile of an already-slow workspace. Instead the tree is
//! checked in and [`super::reference_docs_tests::cli_reference_docs_are_current`]
//! fails when it drifts, so generation cost is paid only when someone
//! deliberately regenerates:
//!
//! ```sh
//! HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs
//! ```
//!
//! # Why it does not call `Command::build`
//!
//! Everything here reads the tree exactly as `clap_derive` produced it.
//! `clap_derive` already emits an explicit `ArgAction` and `value_name` for
//! every field, so building buys nothing this module needs — and it costs two
//! things it does not want. It propagates every `global = true` argument into
//! all ~500 nodes, which this module strips again anyway; and it runs clap's
//! `debug_assert` suite over the whole tree, which turns any pre-existing
//! definition defect anywhere in the CLI into a *docs* test failure. That is not
//! hypothetical: `runner refresh-plan` declares a local `--output` that collides
//! with the propagated global `--output`, and building trips
//! `Long option names must be unique for each argument`. Once that is fixed, a
//! deliberate `build()`-based assertion canary would be a good separate guard;
//! it should not be smuggled in as a side effect of generating docs.
//!
//! # Why it does not overwrite `docs/commands/`
//!
//! `docs/commands/*.md` is hand-written narrative: concepts, recipes, contracts,
//! and migration tables that clap cannot derive. This module writes to a
//! disjoint directory and links back to the narrative page; nothing hand-written
//! is read, rewritten, or deleted.

use super::Cli;
use clap::{Arg, ArgAction, Command, CommandFactory};
use std::collections::BTreeMap;

/// Repo-root-relative directory owned entirely by this generator.
pub(super) const GENERATED_DIR: &str = "docs/reference/cli/commands";

/// Set to any value to rewrite [`GENERATED_DIR`] instead of asserting currency.
pub(super) const WRITE_ENV: &str = "HOMEBOY_WRITE_CLI_REFERENCE";

const BANNER: &str = "<!-- GENERATED FILE. DO NOT EDIT BY HAND.\n\
     Source of truth: the clap command tree in `crates/homeboy-cli`.\n\
     Regenerate with:\n\
     HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib cli_surface::reference_docs\n\
     Hand-written narrative for these commands lives in `docs/commands/`. -->\n\n";

const MISSING_DESCRIPTION: &str =
    "_This command declares no clap help text, so no description can be generated for it._\n\n";

const NO_HELP_CELL: &str = "_no help text_";

/// Renders the full generated tree as `file name -> markdown body`.
pub(super) fn generated_reference_docs() -> BTreeMap<String, String> {
    let root = Cli::command();

    let mut files = BTreeMap::new();
    let mut summaries = Vec::new();
    let mut undocumented = Vec::new();
    let mut node_count = 0usize;

    for command in documented_subcommands(&root) {
        let name = command.get_name().to_string();
        node_count += count_nodes(command);
        commands_without_description(&[name.clone()], command, &mut undocumented);
        summaries.push((
            name.clone(),
            styled(command.get_about())
                .map(|about| cell(&about))
                .unwrap_or_else(|| NO_HELP_CELL.to_string()),
        ));
        files.insert(format!("{name}.md"), render_command_page(&name, command));
    }

    files.insert(
        "index.md".to_string(),
        render_index(&summaries, node_count, &undocumented),
    );
    files
}

fn render_command_page(name: &str, command: &Command) -> String {
    let mut out = String::from(BANNER);
    out.push_str(&format!("# `homeboy {name}` command reference\n\n"));
    out.push_str(
        "Generated from the clap command tree. This page is the complete synopsis, \
         argument, flag, and subcommand surface for this command family.\n\n",
    );

    if let Some(slug) = narrative_slug(name) {
        out.push_str(&format!(
            "Concepts, recipes, and contracts are hand-written in \
             [docs/commands/{slug}.md](../../../commands/{slug}.md).\n\n"
        ));
    }

    out.push_str(
        "Global flags apply to every command and are documented once in \
         [the root command reference](../homeboy-root-command.md).\n\n",
    );

    render_node(&mut out, &[name.to_string()], command);
    out
}

fn render_node(out: &mut String, path: &[String], command: &Command) {
    let display = path.join(" ");
    out.push_str(&format!("## `homeboy {display}`\n\n"));

    let aliases = command
        .get_visible_aliases()
        .map(|alias| format!("`{alias}`"))
        .collect::<Vec<_>>();
    if !aliases.is_empty() {
        out.push_str(&format!("Aliases: {}\n\n", aliases.join(", ")));
    }

    out.push_str("```sh\n");
    out.push_str(&synopsis(path, command));
    out.push_str("\n```\n\n");

    match description(command) {
        Some(text) => {
            out.push_str(&text);
            out.push_str("\n\n");
        }
        None => out.push_str(MISSING_DESCRIPTION),
    }

    let positionals = documented_positionals(command);
    if !positionals.is_empty() {
        out.push_str("| Argument | Required | Description |\n| --- | --- | --- |\n");
        for arg in positionals {
            let placeholder = positional_synopsis(arg);
            let required = if arg.is_required_set() { "yes" } else { "no" };
            let help = help_cell(arg);
            out.push_str(&format!("| `{placeholder}` | {required} | {help} |\n"));
        }
        out.push('\n');
    }

    let options = documented_options(command);
    if !options.is_empty() {
        out.push_str("| Option | Value | Description |\n| --- | --- | --- |\n");
        for arg in options {
            let label = option_label(arg);
            let value = match value_placeholder(arg) {
                Some(placeholder) => format!("`{placeholder}`"),
                None => "flag".to_string(),
            };
            let help = help_cell(arg);
            out.push_str(&format!("| {label} | {value} | {help} |\n"));
        }
        out.push('\n');
    }

    let subcommands = documented_subcommands(command);
    if !subcommands.is_empty() {
        out.push_str("| Subcommand | Summary |\n| --- | --- |\n");
        for subcommand in subcommands.iter().copied() {
            let name = subcommand.get_name();
            let summary = styled(subcommand.get_about())
                .map(|about| cell(&about))
                .unwrap_or_else(|| NO_HELP_CELL.to_string());
            out.push_str(&format!("| `homeboy {display} {name}` | {summary} |\n"));
        }
        out.push('\n');
    }

    for subcommand in subcommands {
        let mut child = path.to_vec();
        child.push(subcommand.get_name().to_string());
        render_node(out, &child, subcommand);
    }
}

fn render_index(
    summaries: &[(String, String)],
    node_count: usize,
    undocumented: &[String],
) -> String {
    let mut out = String::from(BANNER);
    out.push_str("# Homeboy CLI reference (generated)\n\n");
    out.push_str(&format!(
        "`homeboy` exposes {node_count} visible commands across {} top-level command \
         families. Every page below is generated from the clap command tree in \
         `crates/homeboy-cli`, so it cannot drift from the binary.\n\n",
        summaries.len()
    ));
    out.push_str(
        "Hand-written narrative lives in the \
         [commands index](../../../commands/commands-index.md). Global flags are \
         documented in [the root command reference](../homeboy-root-command.md). \
         Machine-readable safety, docs, output, and Lab metadata come from \
         `homeboy contract manifest`.\n\n",
    );

    out.push_str("| Command | Reference | Summary |\n| --- | --- | --- |\n");
    for (name, summary) in summaries {
        out.push_str(&format!(
            "| `homeboy {name}` | [{name}.md]({name}.md) | {summary} |\n"
        ));
    }
    out.push('\n');

    out.push_str("## Commands shipping without help text\n\n");
    if undocumented.is_empty() {
        out.push_str("None. Every visible command declares a clap `about` string.\n");
        return out;
    }

    out.push_str(&format!(
        "{} visible commands declare no clap `about`/`long_about`, so no description \
         can be generated for them. The fix is a doc comment on the clap variant, not \
         prose in this file.\n\n",
        undocumented.len()
    ));
    for path in undocumented {
        out.push_str(&format!("- `homeboy {path}`\n"));
    }
    out
}

fn synopsis(path: &[String], command: &Command) -> String {
    let mut parts = vec!["homeboy".to_string()];
    parts.extend(path.iter().cloned());

    if !documented_options(command).is_empty() {
        parts.push("[OPTIONS]".to_string());
    }
    for arg in documented_positionals(command) {
        parts.push(positional_synopsis(arg));
    }
    if !documented_subcommands(command).is_empty() {
        parts.push(if command.is_subcommand_required_set() {
            "<COMMAND>".to_string()
        } else {
            "[COMMAND]".to_string()
        });
    }

    parts.join(" ")
}

/// Visible child commands, minus clap's generated `help` subcommand.
pub(super) fn documented_subcommands(command: &Command) -> Vec<&Command> {
    command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .filter(|subcommand| subcommand.get_name() != "help")
        .collect()
}

fn documented_positionals(command: &Command) -> Vec<&Arg> {
    command
        .get_positionals()
        .filter(|arg| !arg.is_hide_set())
        .collect()
}

/// Non-positional, non-hidden, non-global arguments.
///
/// Globals are deliberately excluded: they are identical on every node and are
/// pinned as a wire protocol by `root_global_flag_surface_is_pinned`. Repeating
/// them on ~500 pages would bury the per-command surface and would make an
/// unrelated global-flag change rewrite the entire generated tree.
fn documented_options(command: &Command) -> Vec<&Arg> {
    command
        .get_arguments()
        .filter(|arg| !arg.is_positional())
        .filter(|arg| !arg.is_hide_set())
        .filter(|arg| !arg.is_global_set())
        .filter(|arg| !matches!(arg.get_id().as_str(), "help" | "version"))
        .collect()
}

/// Records every visible command node under `path` that has no help text.
pub(super) fn commands_without_description(
    path: &[String],
    command: &Command,
    out: &mut Vec<String>,
) {
    if description(command).is_none() {
        out.push(path.join(" "));
    }

    for subcommand in documented_subcommands(command) {
        let mut child = path.to_vec();
        child.push(subcommand.get_name().to_string());
        commands_without_description(&child, subcommand, out);
    }
}

fn count_nodes(command: &Command) -> usize {
    1 + documented_subcommands(command)
        .into_iter()
        .map(count_nodes)
        .sum::<usize>()
}

fn narrative_slug(name: &str) -> Option<&'static str> {
    crate::command_contract::registered_command(name).and_then(|spec| spec.docs_slug)
}

fn description(command: &Command) -> Option<String> {
    styled(command.get_long_about())
        .or_else(|| styled(command.get_about()))
        .map(|text| paragraphs(&text))
}

fn positional_synopsis(arg: &Arg) -> String {
    let name = arg
        .get_value_names()
        .and_then(|names| names.first())
        .map(|name| name.to_string())
        .unwrap_or_else(|| default_value_name(arg));
    let repeated = if matches!(arg.get_action(), ArgAction::Append) {
        "..."
    } else {
        ""
    };

    if arg.is_required_set() {
        format!("<{name}>{repeated}")
    } else {
        format!("[{name}]{repeated}")
    }
}

fn option_label(arg: &Arg) -> String {
    let mut label = String::new();
    if let Some(short) = arg.get_short() {
        label.push_str(&format!("`-{short}`, "));
    }
    match arg.get_long() {
        Some(long) => label.push_str(&format!("`--{long}`")),
        None => label.push_str(&format!("`{}`", arg.get_id().as_str())),
    }
    label
}

fn value_placeholder(arg: &Arg) -> Option<String> {
    if !matches!(arg.get_action(), ArgAction::Set | ArgAction::Append) {
        return None;
    }

    match arg.get_value_names() {
        Some(names) if !names.is_empty() => Some(
            names
                .iter()
                .map(|name| format!("<{name}>"))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => Some(format!("<{}>", default_value_name(arg))),
    }
}

fn default_value_name(arg: &Arg) -> String {
    arg.get_id().as_str().to_uppercase().replace('_', "-")
}

fn help_cell(arg: &Arg) -> String {
    let mut text = styled(arg.get_help())
        .or_else(|| styled(arg.get_long_help()))
        .map(|help| cell(&help))
        .unwrap_or_else(|| NO_HELP_CELL.to_string());

    // Only value-taking arguments have a meaningful enum. A `SetTrue` flag
    // reports `true`/`false` from its bool value parser, which is noise.
    if value_placeholder(arg).is_some() {
        let values = arg
            .get_possible_values()
            .into_iter()
            .filter(|value| !value.is_hide_set())
            .map(|value| format!("`{}`", value.get_name()))
            .collect::<Vec<_>>();
        if !values.is_empty() {
            text.push_str(&format!(" Values: {}.", values.join(", ")));
        }
    }

    text
}

fn styled(text: Option<&clap::builder::StyledStr>) -> Option<String> {
    text.map(|value| value.to_string())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Collapses hard-wrapped doc comments into markdown paragraphs so the rendered
/// prose reflows instead of inheriting Rust's 80-column source wrapping.
fn paragraphs(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Flattens text into a single markdown table cell.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}
