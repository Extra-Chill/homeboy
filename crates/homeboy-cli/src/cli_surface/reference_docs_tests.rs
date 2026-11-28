//! Coverage guards for runtime-derived command documentation.

use super::reference_docs::{
    commands_without_description, documented_subcommands, generated_command_index,
    generated_command_reference,
};
use super::Cli;

#[test]
fn runner_job_list_is_discoverable_in_help_and_command_index() {
    let root = Cli::command_with_scoped_lab_args();
    let runner_help = generated_command_reference(&root, "runner").expect("runner help");
    assert!(runner_help.contains("homeboy runner job list <runner-id>"));

    let index = generated_command_index(&root);
    assert!(index.lines().any(|line| {
        line.starts_with("- [runner](runner.md)") && line.contains("`runner job list`")
    }));

    let matches = root
        .try_get_matches_from(["homeboy", "runner", "job", "list", "example", "--json"])
        .expect("canonical job list syntax");
    let list = matches
        .subcommand_matches("runner")
        .and_then(|runner| runner.subcommand_matches("job"))
        .and_then(|job| job.subcommand_matches("list"))
        .expect("runner job list matches");
    assert_eq!(list.get_one::<String>("runner_id").unwrap(), "example");
    assert!(list.get_flag("json"));
}

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
