//! Admission-time validation for deterministic gate command contracts.
//!
//! Shell gates remain opaque. This layer interprets only the explicitly owned
//! `homeboy` executable invocation against the installed command surface.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::Serialize;

use homeboy::core::{Error, Result};

use clap::error::ErrorKind;
use clap::{ArgAction, Command};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct GateContractValidation {
    pub schema: &'static str,
    pub status: &'static str,
    pub gates: Vec<GateContractValidationEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct GateContractValidationEntry {
    pub command: String,
    pub kind: &'static str,
    pub status: &'static str,
}

pub(crate) fn validate_gate_contracts(
    gates: impl IntoIterator<Item = String>,
    workspace: Option<&Path>,
    command_contract: &Command,
) -> Result<GateContractValidation> {
    let aliases = repository_script_aliases(workspace)?;
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for command in gates {
        if !seen.insert(command.clone()) {
            continue;
        }
        let Some(argv) = exact_simple_homeboy_invocation(&command) else {
            entries.push(entry(command, "external", "unvalidated"));
            continue;
        };
        let path = command_path(&argv, command_contract);
        if command_contract.clone().try_get_matches_from(argv).is_ok() {
            entries.push(entry(command, "homeboy", "syntax_valid"));
            continue;
        }
        let parse_error = command_contract
            .clone()
            .try_get_matches_from(
                exact_simple_homeboy_invocation(&command)
                    .expect("already parsed simple invocation"),
            )
            .expect_err("invalid command contract was checked above");
        let replacement = path
            .first()
            .filter(|name| aliases.contains(*name))
            .map(|capability| format!("homeboy review {capability} --path ."));
        let remediation = replacement.map(|replacement| {
            format!(" `{}` is a repository script identity, not a Homeboy CLI verb; use `{replacement}`.", path[0])
        }).unwrap_or_else(|| {
            if parse_error.kind() == ErrorKind::MissingRequiredArgument {
                " supply the required arguments shown by `homeboy --help`; this gate is incomplete and was not admitted.".to_string()
            } else {
                " inspect `homeboy contract manifest` for commands provided by this installed version.".to_string()
            }
        });
        return Err(Error::validation_invalid_argument(
            "gate declaration",
            format!(
                "declared Homeboy gate `{command}` is invalid: this installed Homeboy CLI has no `{}` command.{remediation}",
                if path.is_empty() { "subcommand".to_string() } else { path.join(" ") }
            ),
            None,
            None,
        ));
    }
    Ok(GateContractValidation {
        schema: "homeboy/gate-contract-validation/v1",
        status: "valid",
        gates: entries,
    })
}

fn entry(command: String, kind: &'static str, status: &'static str) -> GateContractValidationEntry {
    GateContractValidationEntry {
        command,
        kind,
        status,
    }
}

fn exact_simple_homeboy_invocation(command: &str) -> Option<Vec<String>> {
    if command.contains(['|', '&', ';', '<', '>', '`', '$', '(', ')', '\n', '\r']) {
        return None;
    }
    let argv = shlex::split(command)?;
    (argv.first().map(String::as_str) == Some("homeboy")).then_some(argv)
}

fn command_path(argv: &[String], command: &Command) -> Vec<String> {
    let mut path = Vec::new();
    let mut index = 1;
    let mut current = command;
    while let Some(argument) = argv.get(index) {
        if argument == "--" {
            break;
        }
        if argument.starts_with('-') {
            let name = argument
                .trim_start_matches('-')
                .split('=')
                .next()
                .unwrap_or_default();
            let global = command.get_arguments().find(|candidate| {
                candidate.is_global_set()
                    && (candidate.get_long() == Some(name)
                        || candidate
                            .get_short()
                            .is_some_and(|short| short.to_string() == name))
            });
            if argument.contains('=')
                || global.is_none_or(|arg| {
                    !matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
                })
            {
                index += 1;
            } else {
                index += 2;
            }
            continue;
        }
        let Some(subcommand) = current
            .get_subcommands()
            .find(|candidate| candidate.get_name() == argument)
        else {
            path.push(argument.clone());
            break;
        };
        path.push(argument.clone());
        current = subcommand;
        index += 1;
    }
    path
}

fn repository_script_aliases(workspace: Option<&Path>) -> Result<BTreeSet<String>> {
    let Some(workspace) = workspace else {
        return Ok(BTreeSet::new());
    };
    let manifest_path = workspace.join("homeboy.json");
    if !manifest_path.is_file() {
        return Ok(BTreeSet::new());
    }
    let manifest = fs::read_to_string(&manifest_path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(manifest_path.display().to_string()))
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest).map_err(|error| {
        Error::validation_invalid_argument(
            "gate declaration",
            format!("invalid repository manifest: {error}"),
            Some(manifest_path.display().to_string()),
            None,
        )
    })?;
    Ok(["lint", "test"]
        .into_iter()
        .filter(|capability| {
            manifest
                .pointer(&format!("/scripts/{capability}"))
                .is_some()
        })
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Arg;
    use tempfile::TempDir;

    fn contract() -> Command {
        Command::new("homeboy")
            .subcommand_required(true)
            .arg(
                Arg::new("placement")
                    .long("placement")
                    .global(true)
                    .num_args(1),
            )
            .subcommand(
                Command::new("review")
                    .subcommand_required(true)
                    .subcommand(Command::new("lint").arg(Arg::new("path").long("path").num_args(1)))
                    .subcommand(Command::new("test")),
            )
            .subcommand(Command::new("extension-quality").arg(Arg::new("project").required(true)))
    }

    #[test]
    fn accepts_builtin_and_extension_commands() {
        let result = validate_gate_contracts(
            [
                "homeboy review lint --path .".to_string(),
                "homeboy extension-quality project".to_string(),
            ],
            None,
            &contract(),
        )
        .unwrap();
        assert_eq!(result.gates.len(), 2);
        assert!(result
            .gates
            .iter()
            .all(|gate| gate.status == "syntax_valid"));
    }

    #[test]
    fn identifies_repository_alias_and_canonical_gate() {
        let workspace = TempDir::new().unwrap();
        fs::write(
            workspace.path().join("homeboy.json"),
            r#"{"scripts":{"lint":["check"]}}"#,
        )
        .unwrap();
        let error = validate_gate_contracts(
            ["homeboy lint component --path .".to_string()],
            Some(workspace.path()),
            &contract(),
        )
        .unwrap_err();
        assert!(error.message.contains("repository script identity"));
        assert!(error.message.contains("homeboy review lint --path ."));
    }

    #[test]
    fn rejects_missing_command_and_version_skew() {
        let error =
            validate_gate_contracts(["homeboy lint component".to_string()], None, &contract())
                .unwrap_err();
        assert!(
            error.message.contains("no `lint` command"),
            "{}",
            error.message
        );
        let error =
            validate_gate_contracts(["homeboy review missing".to_string()], None, &contract())
                .unwrap_err();
        assert!(error.message.contains("no `review missing` command"));
    }

    #[test]
    fn preserves_missing_external_executable_without_executing_it() {
        let result = validate_gate_contracts(
            ["missing-executable --would-run".to_string()],
            None,
            &contract(),
        )
        .unwrap();
        assert_eq!(result.gates[0].kind, "external");
        assert_eq!(result.gates[0].status, "unvalidated");
    }

    #[test]
    fn validates_a_shared_gate_once() {
        let result = validate_gate_contracts(
            [
                "homeboy review lint --path .".to_string(),
                "homeboy review lint --path .".to_string(),
            ],
            None,
            &contract(),
        )
        .unwrap();
        assert_eq!(result.gates.len(), 1);
    }

    #[test]
    fn accepts_documented_global_flags_before_the_subcommand() {
        let result = validate_gate_contracts(
            ["homeboy --placement local review lint".to_string()],
            None,
            &contract(),
        )
        .unwrap();
        assert_eq!(result.gates[0].status, "syntax_valid");
    }

    #[test]
    fn rejects_incomplete_homeboy_command() {
        let error =
            validate_gate_contracts(["homeboy extension-quality".to_string()], None, &contract())
                .unwrap_err();
        assert!(error.message.contains("incomplete"));
    }

    #[test]
    fn marks_compound_shell_as_unvalidated() {
        let result = validate_gate_contracts(
            ["homeboy review lint && echo done".to_string()],
            None,
            &contract(),
        )
        .unwrap();
        assert_eq!(result.gates[0].kind, "external");
        assert_eq!(result.gates[0].status, "unvalidated");
    }
}
