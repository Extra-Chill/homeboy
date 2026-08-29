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
    use clap::Parser;

    use crate::commands::agent_task::AgentTaskCommand;
    use crate::commands::agent_task::AgentTaskFanoutCommand;

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
    fn fanout_cook_batch_accepts_preview_and_hidden_dry_run_alias() {
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
        let mut names: Vec<String> = preview
            .get_aliases()
            .into_iter()
            .flatten()
            .map(str::to_string)
            .collect();
        names.sort();
        assert_eq!(names, vec!["dry-run".to_string()]);
        assert!(
            preview
                .get_visible_aliases()
                .map_or(true, |aliases| aliases.is_empty()),
            "--dry-run stays a hidden alias so help advertises one canonical verb"
        );
    }

    #[test]
    fn fanout_plan_parses_from_repo_and_issue_urls_like_cook_batch() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "plan",
            "--repo",
            "owner/repo",
            "https://example.test/issues/1",
        ])
        .expect("fanout plan accepts --repo plus issue URLs");
        let crate::cli_surface::Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
            panic!("fanout command");
        };
        let AgentTaskFanoutCommand::Plan(plan) = fanout.command else {
            panic!("plan command");
        };
        assert_eq!(plan.repo.as_deref(), Some("owner/repo"));
        assert_eq!(
            plan.issues,
            vec!["https://example.test/issues/1".to_string()]
        );

        // --input remains the persisted-plan surface and excludes the
        // issue-planning inputs.
        assert!(
            crate::cli_surface::Cli::try_parse_from([
                "homeboy",
                "agent-task",
                "fanout",
                "plan",
                "--input",
                "plan.json",
                "--repo",
                "owner/repo",
                "https://example.test/issues/1",
            ])
            .is_err(),
            "--input and --repo/ISSUE_URL planning are mutually exclusive"
        );
        assert!(
            crate::cli_surface::Cli::try_parse_from(["homeboy", "agent-task", "fanout", "plan",])
                .is_err(),
            "fanout plan still requires an input: --input SPEC or --repo with issue URLs"
        );
    }
}
