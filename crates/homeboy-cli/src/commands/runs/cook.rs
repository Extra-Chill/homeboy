//! Routing to the single durable Cook readers shared with the domain status view.
use homeboy::agents::agent_task_lifecycle::{self, AgentTaskLifecycleStore};

pub(crate) struct LifecycleReadArgs {
    pub run_id: String,
    pub full: bool,
    pub show: Option<String>,
    pub tail: Option<usize>,
}

pub(crate) struct EvidenceArgs {
    pub run_id: String,
    pub kind: Option<String>,
    pub task: Option<String>,
    pub failure_only: bool,
    pub full: bool,
}

impl super::RunsArgs {
    pub(crate) fn cook_discovery(&self) -> bool {
        matches!(&self.command, super::types::RunsCommand::List(args) if args.kind.as_deref() == Some("agent-task"))
    }

    /// Read-only Cook surfaces retain the former lifecycle-reader routing policy.
    pub(crate) fn cook_reader_id(&self) -> Option<&str> {
        let id = match &self.command {
            super::types::RunsCommand::Artifacts(args) => args.run_id.as_str(),
            super::types::RunsCommand::Evidence { run_id, .. } => run_id.as_str(),
            _ => return None,
        };
        (id.starts_with("agent-task-") || owns(id).unwrap_or(false)).then_some(id)
    }
}

pub(crate) fn owns(id: &str) -> homeboy::core::Result<bool> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    Ok(store.cook_index_exists(id)
        || agent_task_lifecycle::run_record_exists_readonly_in_store(&store, id)?)
}

/// Validate kind-specific flags even when another kind was explicitly supplied.
pub(crate) fn validate_discovery(args: &super::RunsListArgs) -> homeboy::core::Result<()> {
    use homeboy::core::Error;
    let cook = args.kind.as_deref() == Some("agent-task");
    if !cook
        && (args.active
            || args.repo.is_some()
            || args.task_url.is_some()
            || args.run_placement.is_some()
            || args.parent_id.is_some()
            || args.branch.is_some()
            || args.cursor.is_some()
            || args.full)
    {
        return Err(Error::validation_invalid_argument(
            "kind",
            "Cook discovery flags require --kind agent-task",
            args.kind.clone(),
            None,
        ));
    }
    if cook {
        if args.runner.is_some()
            || args.component_id.is_some()
            || args.rig.is_some()
            || args.scenario_id.is_some()
            || args.running
            || args.until.is_some()
            || args.id.is_some()
            || args.command_contains.is_some()
            || args.correlation.is_some()
            || args.include_mirrors
            || args.include_active_runner_jobs
        {
            return Err(Error::validation_invalid_argument("kind", "observation-only filters are not supported with --kind agent-task; use Cook discovery filters", args.kind.clone(), None));
        }
        if args.limit <= 0 {
            return Err(Error::validation_invalid_argument(
                "limit",
                "must be greater than zero",
                None,
                None,
            ));
        }
        if let Some(state) = &args.status {
            if !["queued", "running", "succeeded", "failed", "cancelled"].contains(&state.as_str())
            {
                return Err(Error::validation_invalid_argument(
                    "status",
                    "expected queued, running, succeeded, failed, or cancelled",
                    Some(state.clone()),
                    None,
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn discover(mut args: super::RunsListArgs) -> super::CmdResult<serde_json::Value> {
    use crate::commands::agent_task::status;
    use homeboy::agents::agent_task_service::{
        AgentTaskDiscoveryFilter, AgentTaskDiscoveryOptions,
    };
    if let Some(since) = &args.since {
        if chrono::DateTime::parse_from_rfc3339(since).is_err() {
            args.since = Some(super::common::since_threshold(since)?);
        }
    }
    if args.full {
        let options = AgentTaskDiscoveryOptions {
            limit: None,
            repo: args.repo,
            workspace: args.workspace,
            task_url: args.task_url,
            submitted_after: args.since,
            state: args.status,
            placement: args.run_placement,
            parent_id: args.parent_id,
            branch: args.branch,
            ..Default::default()
        };
        if args.active {
            status::list_active(options)
        } else {
            status::list_runs(AgentTaskDiscoveryFilter::All, options)
        }
    } else if args.active {
        status::list_active_page(args)
    } else {
        status::list_runs_page(AgentTaskDiscoveryFilter::All, args)
    }
}

#[cfg(test)]
mod tests {
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn discovery_cli_selectors_and_removed_verbs() {
        for verb in ["list", "active"] {
            assert!(Cli::try_parse_from(["homeboy", "agent-task", verb]).is_err());
        }
        assert!(Cli::try_parse_from(["homeboy", "agent-task", "reconcile"]).is_err());
        assert!(Cli::try_parse_from(["homeboy", "agent-task", "reconcile", "--all"]).is_ok());
        assert!(
            Cli::try_parse_from(["homeboy", "agent-task", "reconcile", "id", "--all"]).is_err()
        );
        for flag in [
            "--repo",
            "--task-url",
            "--run-placement",
            "--parent-id",
            "--branch",
            "--cursor",
            "--full",
            "--active",
        ] {
            let mut argv = vec!["homeboy", "runs", "list", flag];
            if !["--full", "--active"].contains(&flag) {
                argv.push(if flag == "--run-placement" {
                    "local"
                } else {
                    "value"
                });
            }
            assert!(Cli::try_parse_from(&argv).is_err(), "{flag}");
            argv.extend(["--kind", "bench"]);
            let Commands::Runs(args) = Cli::try_parse_from(argv).unwrap().command else {
                unreachable!()
            };
            let super::super::types::RunsCommand::List(list) = args.command else {
                unreachable!()
            };
            assert!(super::validate_discovery(&list).is_err());
        }
        for flag in [
            "--repo",
            "--task-url",
            "--workspace",
            "--since",
            "--status",
            "--run-placement",
            "--parent-id",
        ] {
            assert!(Cli::try_parse_from([
                "homeboy",
                "runs",
                "list",
                "--kind",
                "agent-task",
                "--active",
                flag,
                "value"
            ])
            .is_err());
        }
        for state in [
            "queued",
            "running",
            "succeeded",
            "failed",
            "cancelled",
            "invalid",
        ] {
            let Commands::Runs(args) = Cli::try_parse_from([
                "homeboy",
                "runs",
                "list",
                "--kind",
                "agent-task",
                "--status",
                state,
            ])
            .unwrap()
            .command
            else {
                unreachable!()
            };
            let super::super::types::RunsCommand::List(list) = args.command else {
                unreachable!()
            };
            assert_eq!(super::validate_discovery(&list).is_ok(), state != "invalid");
        }
    }
}

#[cfg(test)]
mod pagination_parse_tests {
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;
    #[test]
    fn active_continuation_and_fleet_scopes_are_separate() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "runs",
            "list",
            "--kind",
            "agent-task",
            "--active",
            "--limit",
            "20",
            "--cursor",
            "opaque",
        ])
        .unwrap();
        let Commands::Runs(args) = cli.command else {
            unreachable!()
        };
        let super::super::types::RunsCommand::List(args) = args.command else {
            unreachable!()
        };
        assert_eq!(args.limit, 20);
        assert_eq!(args.cursor.as_deref(), Some("opaque"));
        for flag in ["--limit", "--cursor", "--branch", "--full"] {
            assert!(Cli::try_parse_from([
                "homeboy",
                "agent-task",
                "reconcile",
                "--all",
                flag,
                "1"
            ])
            .is_err());
        }
    }
}
