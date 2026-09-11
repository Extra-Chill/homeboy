//! Coverage guards for runtime-derived command documentation.

use super::reference_docs::{commands_without_description, documented_subcommands};
use super::Cli;

/// A command with no clap `about` is invisible in `--help` and to any agent
/// reading the command surface.
#[test]
fn every_visible_command_ships_help_text() {
    let root = Cli::command_with_scoped_lab_args();

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
