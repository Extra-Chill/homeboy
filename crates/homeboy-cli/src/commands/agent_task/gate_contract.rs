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

/// #14731: resolve each declared gate against the placement this process
/// already resolved for this Cook, and fail admission when a gate cannot
/// execute there — before a provider is dispatched whose work would be
/// discarded when the gate defers at execution time.
///
/// This reuses the same preflight decision preview and dispatch both already
/// read (`parsed_command_preflight::captured_result`), so it costs no
/// additional live I/O and cannot disagree with what execution actually does.
///
/// Recognizes exactly the `homeboy review test` declared-test prefix — the
/// same prefix `TestExecutionPlan::declared_homeboy_review_test` requires,
/// and Cook's own documented default gate (`homeboy --help` quick start). It
/// is a `portable_lab_route` command: with no ready Lab runner it defers
/// rather than executing, which is exactly the substitution this rejects
/// before it can consume a provider execution.
pub(crate) fn reject_gates_unexecutable_under_resolved_placement(
    gates: impl IntoIterator<Item = String>,
) -> Result<()> {
    let Some(result) = homeboy::core::parsed_command_preflight::captured_result() else {
        return Ok(());
    };
    let selected_local = result.placement.selected
        == homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local;
    let lab_ready = result
        .lab_readiness
        .as_ref()
        .is_some_and(|readiness| readiness.state == "connected_ready");
    reject_gates_unexecutable_under_placement(gates, selected_local, lab_ready)
}

/// Pure admission check, separated from its process-global-reading caller so
/// it can be exercised deterministically without touching the shared
/// `parsed_command_preflight` capture slot (process-wide, not safe to mutate
/// from parallel tests).
fn reject_gates_unexecutable_under_placement(
    gates: impl IntoIterator<Item = String>,
    selected_local: bool,
    lab_ready: bool,
) -> Result<()> {
    if !selected_local || lab_ready {
        return Ok(());
    }
    let Some(gate) = gates
        .into_iter()
        .find(|command| gate_requires_portable_lab_route(command))
    else {
        return Ok(());
    };
    Err(Error::validation_invalid_argument(
        "verify",
        format!(
            "declared gate `{gate}` requires a Lab route and cannot execute under the resolved local placement; dispatching a provider now would discard its work when this gate defers at execution time"
        ),
        None,
        Some(vec![
            "Connect a ready Lab runner before dispatching (see `homeboy runner status`), or replace the gate with one that can execute locally.".to_string(),
        ]),
    ))
}

fn gate_requires_portable_lab_route(command: &str) -> bool {
    exact_simple_homeboy_invocation(command)
        .is_some_and(|argv| argv.len() >= 3 && argv[1] == "review" && argv[2] == "test")
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

    /// #14731: admission must resolve `homeboy review test` — Cook's own
    /// documented default gate — against the selected placement and refuse it
    /// *before* a provider is dispatched, rather than discovering at
    /// execution time that the gate deferred and the provider's work was
    /// wasted.
    #[test]
    fn rejects_a_lab_routed_gate_when_local_is_selected_and_no_lab_runner_is_ready() {
        let error = reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy".to_string()],
            true,
            false,
        )
        .expect_err("a portable-lab-route gate cannot execute locally with no ready runner");
        assert!(error.message.contains("homeboy review test homeboy"));
        assert!(error
            .message
            .contains("cannot execute under the resolved local placement"));
    }

    #[test]
    fn admits_the_gate_when_a_lab_runner_is_ready() {
        reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy".to_string()],
            true,
            true,
        )
        .expect("a ready Lab runner admits the same gate");
    }

    #[test]
    fn admits_the_gate_when_placement_selected_lab() {
        reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy".to_string()],
            false,
            false,
        )
        .expect("a Lab-selected placement admits the gate regardless of this local-only check");
    }

    #[test]
    fn does_not_classify_unrelated_gates_as_lab_routed() {
        reject_gates_unexecutable_under_placement(
            ["homeboy review lint --path .".to_string()],
            true,
            false,
        )
        .expect("only the recognized `homeboy review test` prefix is rejected here");
    }
}
