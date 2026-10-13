//! Generic help-text guards for the `agent-task` command tree.
//!
//! #13704: the two flags deciding whether a wave of cooks actually runs were
//! the only undocumented ones on `fanout cook-batch`. Per-flag tests only fix
//! today's typo; this walk fails for ANY future argument that ships with
//! empty help text anywhere under `agent-task`.

#[cfg(test)]
mod tests {
    use clap::Command;
    use clap::CommandFactory;

    fn agent_task_command() -> Command {
        crate::cli_surface::Cli::command()
            .find_subcommand("agent-task")
            .expect("agent-task command exists")
            .clone()
    }

    fn argument_display_name(arg: &clap::Arg) -> String {
        if arg.is_positional() {
            format!(
                "<{}>",
                arg.get_value_names()
                    .map(|names| names.join(" "))
                    .unwrap_or_else(|| arg.get_id().as_str().to_string())
            )
        } else {
            arg.get_long()
                .map(|long| format!("--{long}"))
                .unwrap_or_else(|| {
                    arg.get_short()
                        .map(|short| format!("-{short}"))
                        .unwrap_or_else(|| arg.get_id().as_str().to_string())
                })
        }
    }

    fn audit_command(command: &Command, path: String, failures: &mut Vec<String>) {
        for arg in command.get_arguments() {
            // Hidden arguments never render, auto help/version flags carry
            // clap's built-in text, and global flags are documented at the
            // root surface that owns them.
            if arg.is_hide_set() || arg.is_global_set() {
                continue;
            }
            if matches!(arg.get_id().as_str(), "help" | "version") {
                continue;
            }
            let documented = arg
                .get_help()
                .is_some_and(|help| !help.to_string().trim().is_empty());
            if !documented {
                failures.push(format!("{path} {}", argument_display_name(arg)));
            }
        }
        for subcommand in command.get_subcommands() {
            if subcommand.is_hide_set() || subcommand.get_name() == "help" {
                continue;
            }
            let sub_path = format!("{path} {}", subcommand.get_name());
            let about_is_blank = subcommand
                .get_about()
                .is_some_and(|about| about.to_string().trim().is_empty());
            if about_is_blank {
                failures.push(format!("{sub_path} <missing one-line about>"));
            }
            audit_command(subcommand, sub_path, failures);
        }
    }

    #[test]
    fn no_agent_task_argument_ships_with_empty_help_text() {
        let root = agent_task_command();
        let mut failures = Vec::new();
        audit_command(&root, "agent-task".to_string(), &mut failures);
        assert!(
            failures.is_empty(),
            "agent-task arguments with empty help text (every visible flag and \
             positional must explain itself):\n  {}",
            failures.join("\n  ")
        );
    }

    #[test]
    fn fanout_cook_batch_accepts_preview_without_dry_run_alias() {
        let command = agent_task_command();
        let cook_batch = command
            .find_subcommand("fanout")
            .expect("fanout command")
            .find_subcommand("cook-batch")
            .expect("cook-batch command");
        let preview = cook_batch
            .get_arguments()
            .find(|arg| arg.get_long() == Some("preview"))
            .expect("cook-batch exposes --preview");
        assert!(
            preview
                .get_help()
                .is_some_and(|help| !help.to_string().trim().is_empty()),
            "--preview must carry help text"
        );
        // The historical `--dry-run` spelling was deleted (#15742); `--preview`
        // is the one verb.
        assert!(
            preview.get_all_aliases().is_none(),
            "--preview carries no aliases"
        );
    }

}
