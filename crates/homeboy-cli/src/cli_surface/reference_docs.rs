//! Runtime-derived command documentation.

use clap::Command;

pub(crate) fn generated_command_index(command: &Command) -> String {
    let mut commands = documented_subcommands(command)
        .into_iter()
        .map(|command| command.get_name().to_string())
        .collect::<Vec<_>>();
    commands.sort_unstable();

    let mut out = String::from(
        "# Commands index\n\n\
         This index is generated on demand from the command surface in this Homeboy build.\n\n",
    );
    for command in commands {
        out.push_str(&format!("- [{command}]({command}.md)\n"));
    }
    out.push_str(
        "\nRun `homeboy <command> --help` for the current argument and subcommand surface. \
         Machine-readable safety, documentation, output, and Lab metadata are available from \
         `homeboy contract manifest`.\n",
    );
    out
}

pub(crate) fn generated_command_reference(command: &Command, name: &str) -> Option<String> {
    let mut command = command
        .find_subcommand(name)?
        .clone()
        .bin_name(format!("homeboy {name}"));
    let mut output = Vec::new();
    command.write_long_help(&mut output).ok()?;
    String::from_utf8(output).ok()
}

pub(crate) fn documented_subcommands(command: &Command) -> Vec<&Command> {
    command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .filter(|subcommand| subcommand.get_name() != "help")
        .collect()
}

#[cfg(test)]
pub(super) fn commands_without_description(
    path: &[String],
    command: &Command,
    out: &mut Vec<String>,
) {
    if command
        .get_long_about()
        .or_else(|| command.get_about())
        .is_none()
    {
        out.push(path.join(" "));
    }

    for subcommand in documented_subcommands(command) {
        let mut child = path.to_vec();
        child.push(subcommand.get_name().to_string());
        commands_without_description(&child, subcommand, out);
    }
}
