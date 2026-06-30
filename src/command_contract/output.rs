//! Output contract types and routing logic.
//!
//! This module owns the shapes that describe how a command emits output
//! (`CommandResponseMode`, `CommandStdoutMode`, `CommandOutputFileMode`,
//! `CommandJsonFamily`, `CommandOutputContractKind`) plus the output-only
//! [`CommandOutputDescriptor`], aggregate [`CommandDescriptor`],
//! [`CommandResponsePlan`], and the `Commands` impl that resolves a parsed CLI
//! command into these contracts.
//!
//! Lab-specific fields on [`CommandDescriptor`] are populated by
//! [`crate::command_contract::lab`], which post-processes the descriptor
//! returned from [`Commands::descriptor`].

use crate::cli_surface::Commands;
use crate::commands::{
    changelog, file, fleet, logs, observe, report, review, runtime, trace, version,
};

use super::lab::apply_lab_contract_to_descriptor;
use super::spec::registered_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResponseMode {
    Json,
    Raw(CommandRawOutputMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRawOutputMode {
    InteractivePassthrough,
    Markdown,
    PlainText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStdoutMode {
    JsonEnvelope,
    Raw(CommandRawOutputMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputFileMode {
    None,
    GenericEnvelope,
    ReviewStableArtifact,
    TraceJsonSummaryArtifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandJsonFamily {
    Quality,
    Workspace,
    Ops,
    RawOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDispatchFamily {
    Quality,
    Workspace,
    Ops,
    RawOnly,
}

impl From<CommandJsonFamily> for CommandDispatchFamily {
    fn from(json_family: CommandJsonFamily) -> Self {
        match json_family {
            CommandJsonFamily::Quality => CommandDispatchFamily::Quality,
            CommandJsonFamily::Workspace => CommandDispatchFamily::Workspace,
            CommandJsonFamily::Ops => CommandDispatchFamily::Ops,
            CommandJsonFamily::RawOnly => CommandDispatchFamily::RawOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutputContractKind {
    JsonEnvelope,
    RawOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandOutputDescriptor {
    pub response_mode: CommandResponseMode,
    pub output_file_mode: CommandOutputFileMode,
    pub json_family: CommandJsonFamily,
    pub output_contract: CommandOutputContractKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDescriptor {
    /// Shared output-routing spec (response mode, output-file mode, JSON family,
    /// output contract). Factored out of the per-struct field group so the same
    /// shape is reused by [`CommandOutputDescriptor`] and the command adapter
    /// contract.
    pub output: CommandOutputDescriptor,
    pub supports_lab_runner: bool,
    pub lab_runner_unsupported_reason: Option<&'static str>,
    pub lab_offload_captures_mutation_patch: bool,
    pub lab_offload_mutation_flag: Option<&'static str>,
}

impl CommandOutputDescriptor {
    pub const fn json_envelope(
        json_family: CommandJsonFamily,
        output_file_mode: CommandOutputFileMode,
    ) -> Self {
        Self {
            response_mode: CommandResponseMode::Json,
            output_file_mode,
            json_family,
            output_contract: CommandOutputContractKind::JsonEnvelope,
        }
    }

    fn with_lab_contract(
        self,
        contract: Option<super::lab::LabCommandContract>,
    ) -> CommandDescriptor {
        let mut descriptor = CommandDescriptor {
            output: self,
            supports_lab_runner: false,
            lab_runner_unsupported_reason: None,
            lab_offload_captures_mutation_patch: false,
            lab_offload_mutation_flag: None,
        };
        apply_lab_contract_to_descriptor(&mut descriptor, contract);
        descriptor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandResponsePlan {
    pub stdout: CommandStdoutMode,
    pub output_file: CommandOutputFileMode,
}

impl Commands {
    pub fn descriptor(&self, has_output_file: bool) -> CommandDescriptor {
        self.output_descriptor(has_output_file)
            .with_lab_contract(self.lab_contract())
    }

    pub fn output_descriptor(&self, has_output_file: bool) -> CommandOutputDescriptor {
        let output_file_mode = if !has_output_file {
            CommandOutputFileMode::None
        } else {
            match self {
                Commands::Review(_) => CommandOutputFileMode::ReviewStableArtifact,
                Commands::Trace(args) if args.json_summary => {
                    CommandOutputFileMode::TraceJsonSummaryArtifact
                }
                _ => CommandOutputFileMode::GenericEnvelope,
            }
        };

        match self {
            Commands::Ssh(args) if args.subcommand.is_none() && args.command.is_empty() => {
                raw_ops_descriptor(
                    CommandRawOutputMode::InteractivePassthrough,
                    output_file_mode,
                )
            }
            Commands::Logs(args) if logs::is_interactive(args) => raw_ops_descriptor(
                CommandRawOutputMode::InteractivePassthrough,
                output_file_mode,
            ),
            Commands::File(args) if file::is_raw_read(args) => {
                raw_ops_descriptor(CommandRawOutputMode::PlainText, output_file_mode)
            }
            Commands::Docs(args) => workspace_descriptor(
                if crate::commands::docs::is_json_mode(args) {
                    CommandResponseMode::Json
                } else {
                    CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
                },
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Changelog(args) if changelog::is_show_markdown(args) => workspace_descriptor(
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Review(args) => CommandOutputDescriptor {
                response_mode: markdown_or_json_response(review::is_markdown_mode(args)),
                output_file_mode,
                json_family: CommandJsonFamily::Quality,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Trace(args) => CommandOutputDescriptor {
                response_mode: markdown_or_json_response(trace::is_markdown_mode(args)),
                output_file_mode,
                json_family: CommandJsonFamily::Quality,
                output_contract: CommandOutputContractKind::JsonEnvelope,
            },
            Commands::Runs(args) => workspace_descriptor(
                if !has_output_file && args.is_markdown_mode() {
                    CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
                } else {
                    CommandResponseMode::Json
                },
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Runtime(args) if runtime::is_plain_mode(args) => {
                raw_ops_descriptor(CommandRawOutputMode::PlainText, output_file_mode)
            }
            Commands::Report(args) if report::is_markdown_mode(args) => workspace_descriptor(
                CommandResponseMode::Raw(CommandRawOutputMode::Markdown),
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Test(args) => args.output_descriptor(output_file_mode),
            Commands::Bench(args) => args.output_descriptor(output_file_mode),
            Commands::Fuzz(args) => args.output_descriptor(output_file_mode),
            Commands::Lint(args) => args.output_descriptor(output_file_mode),
            Commands::Audit(args) => args.output_descriptor(output_file_mode),
            Commands::Observe(_) => observe::adapter(output_file_mode).output_descriptor(),
            Commands::AuditBaseline(_) | Commands::Refactor(_) => {
                registered_json_envelope_descriptor(self, output_file_mode)
            }
            Commands::Refs(_) => workspace_descriptor(
                CommandResponseMode::Json,
                output_file_mode,
                CommandOutputContractKind::JsonEnvelope,
            ),
            Commands::Version(_) => version::adapter(output_file_mode).output_descriptor(),
            Commands::AgentTask(_)
            | Commands::Project(_)
            | Commands::Component(_)
            | Commands::Config(_)
            | Commands::Contract(_)
            | Commands::Extension(_)
            | Commands::Manifest(_)
            | Commands::Changelog(_)
            | Commands::Cleanup(_)
            | Commands::Build(_)
            | Commands::Changes(_)
            | Commands::Release(_)
            | Commands::Report(_)
            | Commands::Runner(_)
            | Commands::Runtime(_)
            | Commands::Worktree(_)
            | Commands::Tunnel(_)
            | Commands::Stack(_)
            | Commands::Undo(_) => registered_json_envelope_descriptor(self, output_file_mode),
            Commands::Rig(_) => registered_json_envelope_descriptor(self, output_file_mode),
            Commands::Status(_)
            | Commands::Ci(_)
            | Commands::Server(_)
            | Commands::Db(_)
            | Commands::Deps(_)
            | Commands::File(_)
            | Commands::Logs(_)
            | Commands::Deploy(_)
            | Commands::Daemon(_)
            | Commands::Git(_)
            | Commands::Issues(_)
            | Commands::SelfCmd(_)
            | Commands::Auth(_)
            | Commands::Api(_)
            | Commands::Http(_)
            | Commands::Upgrade(_)
            | Commands::Ssh(_) => registered_json_envelope_descriptor(self, output_file_mode),
            Commands::Fleet(_) => fleet::adapter(output_file_mode).output_descriptor(),
            Commands::Triage(_) => registered_json_envelope_descriptor(self, output_file_mode),
        }
    }

    pub fn response_plan(&self, has_output_file: bool) -> CommandResponsePlan {
        let descriptor = self.output_descriptor(has_output_file);

        CommandResponsePlan {
            stdout: match descriptor.response_mode {
                CommandResponseMode::Json => CommandStdoutMode::JsonEnvelope,
                CommandResponseMode::Raw(raw_mode) => CommandStdoutMode::Raw(raw_mode),
            },
            output_file: descriptor.output_file_mode,
        }
    }

    pub fn response_mode(&self, has_output_file: bool) -> CommandResponseMode {
        self.output_descriptor(has_output_file).response_mode
    }

    pub fn output_file_mode(&self, has_output_file: bool) -> CommandOutputFileMode {
        self.output_descriptor(has_output_file).output_file_mode
    }

    pub fn consumes_output_file_as_command_arg(&self) -> bool {
        matches!(self, Commands::Runs(args) if args.is_artifact_get())
    }
}

fn raw_ops_descriptor(
    raw_mode: CommandRawOutputMode,
    output_file_mode: CommandOutputFileMode,
) -> CommandOutputDescriptor {
    CommandOutputDescriptor {
        response_mode: CommandResponseMode::Raw(raw_mode),
        output_file_mode,
        json_family: CommandJsonFamily::Ops,
        output_contract: CommandOutputContractKind::JsonEnvelope,
    }
}

fn workspace_descriptor(
    response_mode: CommandResponseMode,
    output_file_mode: CommandOutputFileMode,
    output_contract: CommandOutputContractKind,
) -> CommandOutputDescriptor {
    CommandOutputDescriptor {
        response_mode,
        output_file_mode,
        json_family: CommandJsonFamily::Workspace,
        output_contract,
    }
}

fn markdown_or_json_response(markdown: bool) -> CommandResponseMode {
    if markdown {
        CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
    } else {
        CommandResponseMode::Json
    }
}

fn registered_json_envelope_descriptor(
    command: &Commands,
    output_file_mode: CommandOutputFileMode,
) -> CommandOutputDescriptor {
    registered_command(command.top_level_name())
        .expect("top-level command should be registered")
        .output_descriptor(output_file_mode)
}

#[cfg(test)]
mod tests {
    use super::super::spec::{COMMAND_REGISTRY, DEFAULT_LAB_UNSUPPORTED_NOTES};
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::CommandFactory;
    use clap::Parser;
    use std::collections::BTreeSet;

    fn parsed_command(args: &[&str]) -> Commands {
        Cli::try_parse_from(args)
            .expect("CLI args should parse")
            .command
    }

    fn lab_supported_registry_sample_argv(command_name: &str) -> Option<&'static [&'static str]> {
        match command_name {
            "agent-task" => Some(&["homeboy", "agent-task", "providers"]),
            "test" => Some(&["homeboy", "test"]),
            "bench" => Some(&["homeboy", "bench"]),
            "fuzz" => Some(&["homeboy", "fuzz"]),
            "trace" => Some(&["homeboy", "trace"]),
            "lint" => Some(&["homeboy", "lint"]),
            "review" => Some(&["homeboy", "review"]),
            "audit" => Some(&["homeboy", "audit"]),
            "refactor" => Some(&["homeboy", "refactor", "--all"]),
            "runtime" => Some(&[
                "homeboy",
                "runtime",
                "refresh",
                "example-runtime",
                "--source",
                ".",
            ]),
            "rig" => Some(&["homeboy", "rig", "check", "example-rig"]),
            "tunnel" => Some(&[
                "homeboy",
                "tunnel",
                "service",
                "start",
                "example-service",
                "--command",
                "npm start",
            ]),
            _ => None,
        }
    }

    #[test]
    fn test_response_mode() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).response_mode(false),
            CommandResponseMode::Json
        );
        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--report", "markdown"]).response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
        assert_eq!(
            parsed_command(&["homeboy", "manifest"]).response_mode(false),
            CommandResponseMode::Json
        );
        assert_eq!(
            parsed_command(&["homeboy", "changelog"]).response_mode(false),
            CommandResponseMode::Raw(CommandRawOutputMode::Markdown)
        );
    }

    #[test]
    fn test_command_descriptor_drives_behavioral_routing() {
        let bench = parsed_command(&["homeboy", "bench"]);
        let bench_descriptor = bench.descriptor(false);
        assert_eq!(
            bench_descriptor.output.json_family,
            CommandJsonFamily::Quality
        );
        assert!(bench_descriptor.supports_lab_runner);
        assert_eq!(
            bench_descriptor.output.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let runs = parsed_command(&["homeboy", "runs", "list"]);
        let runs_descriptor = runs.descriptor(false);
        assert_eq!(
            runs_descriptor.output.json_family,
            CommandJsonFamily::Workspace
        );
        assert_eq!(
            runs_descriptor.output.response_mode,
            CommandResponseMode::Json
        );
        assert_eq!(
            runs_descriptor.output.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let fleet_descriptor = parsed_command(&["homeboy", "fleet", "list"]).descriptor(false);
        assert_eq!(fleet_descriptor.output.json_family, CommandJsonFamily::Ops);
        assert_eq!(
            fleet_descriptor.output.response_mode,
            CommandResponseMode::Json
        );
        assert_eq!(
            fleet_descriptor.output.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let manifest_descriptor = parsed_command(&["homeboy", "manifest"]).descriptor(false);
        assert_eq!(
            manifest_descriptor.output.json_family,
            CommandJsonFamily::Workspace
        );
        assert_eq!(
            manifest_descriptor.output.response_mode,
            CommandResponseMode::Json
        );
    }

    #[test]
    fn command_registry_covers_top_level_parser_surface() {
        let parser_names = Cli::command()
            .get_subcommands()
            .map(|subcommand| subcommand.get_name().to_string())
            .collect::<BTreeSet<_>>();
        let registry_names = COMMAND_REGISTRY
            .iter()
            .map(|entry| entry.name.to_string())
            .collect::<BTreeSet<_>>();

        assert_eq!(registry_names, parser_names);
    }

    #[test]
    fn command_registry_lab_metadata_is_explicit() {
        for entry in COMMAND_REGISTRY {
            if entry.lab_supported {
                assert_ne!(
                    entry.lab_notes, DEFAULT_LAB_UNSUPPORTED_NOTES,
                    "Lab-supported command `{}` should not use the default non-Lab note",
                    entry.name
                );
                assert!(
                    !entry.lab_notes.trim().is_empty(),
                    "Lab-supported command `{}` should explain Lab support",
                    entry.name
                );
            } else {
                assert_eq!(
                    entry.lab_notes, DEFAULT_LAB_UNSUPPORTED_NOTES,
                    "non-Lab command `{}` should use the explicit default non-supported note",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn command_registry_lab_metadata_matches_command_support_for_parseable_samples() {
        for entry in COMMAND_REGISTRY.iter().filter(|entry| entry.lab_supported) {
            let argv = lab_supported_registry_sample_argv(entry.name).unwrap_or_else(|| {
                panic!(
                    "Lab-supported registry command `{}` needs a representative parseable argv sample",
                    entry.name
                )
            });
            let command = parsed_command(argv);
            let descriptor = command.descriptor(false);

            assert_eq!(
                command.top_level_name(),
                entry.name,
                "registry sample argv should parse to the matching top-level command"
            );
            assert_eq!(
                descriptor.supports_lab_runner,
                command.supports_lab_runner(),
                "descriptor Lab support drifted from Commands::supports_lab_runner() for `{}`",
                entry.name
            );
            assert_eq!(
                entry.lab_supported,
                command.supports_lab_runner(),
                "registry Lab metadata drifted from Commands::supports_lab_runner() for `{}`",
                entry.name
            );
        }
    }

    #[test]
    fn command_registry_docs_path_is_present_for_commands_with_docs() {
        for entry in COMMAND_REGISTRY {
            if let Some(slug) = entry.docs_slug {
                assert_eq!(
                    entry.docs_path().as_deref(),
                    Some(format!("docs/commands/{slug}.md").as_str()),
                    "registered command `{}` docs path drifted from docs slug",
                    entry.name
                );
            } else {
                assert!(
                    entry.docs_path().is_none(),
                    "registered command `{}` should not expose docs path without docs slug",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn output_descriptor_excludes_lab_policy() {
        let scoped_lint = parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"]);
        let output_descriptor = scoped_lint.output_descriptor(false);

        assert_eq!(output_descriptor.json_family, CommandJsonFamily::Quality);
        assert_eq!(output_descriptor.response_mode, CommandResponseMode::Json);
        assert_eq!(
            output_descriptor.output_contract,
            CommandOutputContractKind::JsonEnvelope
        );

        let aggregate_descriptor = scoped_lint.descriptor(false);
        assert!(aggregate_descriptor.supports_lab_runner);
        assert!(aggregate_descriptor.lab_runner_unsupported_reason.is_none());
    }

    #[test]
    fn command_spec_output_descriptor_uses_shared_contract_shape() {
        let spec = crate::command_contract::registered_command("status")
            .expect("status command should be registered");

        assert_eq!(
            spec.output_descriptor(CommandOutputFileMode::GenericEnvelope),
            CommandOutputDescriptor::json_envelope(
                CommandJsonFamily::Ops,
                CommandOutputFileMode::GenericEnvelope,
            )
        );
    }

    #[test]
    fn test_response_plan() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).response_plan(false),
            CommandResponsePlan {
                stdout: CommandStdoutMode::JsonEnvelope,
                output_file: CommandOutputFileMode::None,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_plan(false),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::None,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "review", "--report", "pr-comment"]).response_plan(true),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::ReviewStableArtifact,
            }
        );

        assert_eq!(
            parsed_command(&["homeboy", "trace", "--report", "markdown", "--json-summary",])
                .response_plan(true),
            CommandResponsePlan {
                stdout: CommandStdoutMode::Raw(CommandRawOutputMode::Markdown),
                output_file: CommandOutputFileMode::TraceJsonSummaryArtifact,
            }
        );
    }

    #[test]
    fn artifact_get_output_flag_is_command_payload_destination() {
        assert!(parsed_command(&[
            "homeboy",
            "runs",
            "artifact",
            "get",
            "run-1",
            "artifact-1",
            "-o",
            "artifact.bin",
        ])
        .consumes_output_file_as_command_arg());

        assert!(!parsed_command(&["homeboy", "status"]).consumes_output_file_as_command_arg());
    }

    #[test]
    fn test_output_artifact_policy() {
        assert_eq!(
            parsed_command(&["homeboy", "status"]).output_file_mode(true),
            CommandOutputFileMode::GenericEnvelope
        );
        assert_eq!(
            parsed_command(&["homeboy", "review"]).output_file_mode(true),
            CommandOutputFileMode::ReviewStableArtifact
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--json-summary"]).output_file_mode(true),
            CommandOutputFileMode::TraceJsonSummaryArtifact
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--json-summary"]).output_file_mode(false),
            CommandOutputFileMode::None
        );
    }
}
