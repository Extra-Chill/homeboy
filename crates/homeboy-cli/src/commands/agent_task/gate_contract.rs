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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_interpretation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<String>,
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

/// Validate Cargo gate shape without building test binaries. A preview may
/// report focused selections as deferred when it has no trustworthy cached
/// test inventory; runtime gate evidence remains authoritative.
pub(crate) fn validate_cargo_gate_contracts(
    gates: impl IntoIterator<Item = String>,
    _workspace: Option<&Path>,
) -> Result<Vec<GateContractValidationEntry>> {
    let mut entries = Vec::new();
    for command in gates {
        let Some(selection) = cargo_gate_shape(&command)? else {
            continue;
        };
        let deferred_selection = selection.mode == "focused";
        entries.push(GateContractValidationEntry {
            command,
            kind: "cargo",
            status: if deferred_selection {
                "selection_deferred"
            } else {
                "shape_valid"
            },
            mode: Some(selection.mode),
            filter_interpretation: Some(selection.filter_interpretation),
            filter: selection.filter,
            selected_count: None,
            selected_ids: None,
            validation: deferred_selection
                .then(|| "test_inventory_unavailable_without_build".to_string()),
        });
    }
    Ok(entries)
}

struct CargoSelection {
    mode: String,
    filter_interpretation: String,
    filter: Option<String>,
}

fn cargo_gate_shape(command: &str) -> Result<Option<CargoSelection>> {
    let Some(tokens) = shlex::split(command) else {
        return Ok(None);
    };
    let Some(cargo) = tokens.iter().position(|token| token == "cargo") else {
        return Ok(None);
    };
    if tokens.get(cargo + 1).map(String::as_str) != Some("test") {
        return Ok(None);
    }
    let args = &tokens[cargo + 2..];
    let harness = args.iter().position(|token| token == "--");
    let before_harness = &args[..harness.unwrap_or(args.len())];
    let mut filter_index = 0;
    while let Some(argument) = before_harness.get(filter_index) {
        if !argument.starts_with('-') {
            break;
        }
        filter_index += 1;
        if matches!(
            argument.as_str(),
            "-p" | "--package"
                | "--exclude"
                | "--bin"
                | "--example"
                | "--test"
                | "--bench"
                | "--features"
                | "--target"
                | "--target-dir"
                | "--manifest-path"
                | "--profile"
                | "-j"
                | "--jobs"
                | "--config"
                | "--message-format"
                | "--timings"
        ) {
            filter_index += 1;
        }
    }
    let filter = before_harness.get(filter_index).cloned();
    if filter
        .as_deref()
        .is_some_and(|filter| filter.ends_with("::"))
    {
        return Err(Error::validation_invalid_argument(
            "gate declaration",
            format!(
                "declared Cargo test filter `{}` is intrinsically invalid: a focused filter must name a test or module prefix, not end with `::`",
                filter.expect("checked above")
            ),
            None,
            Some(vec![
                "Use an exact test ID with `-- --exact`, or remove the trailing `::` and use a bounded module prefix.".to_string(),
            ]),
        ));
    }
    let exact = harness.is_some_and(|index| args[index + 1..].iter().any(|arg| arg == "--exact"));
    let interpretation = if filter.is_none() {
        "broad_explicit"
    } else if exact {
        "exact"
    } else {
        "candidate_resolved"
    };
    Ok(Some(CargoSelection {
        mode: if filter.is_some() { "focused" } else { "broad" }.to_string(),
        filter_interpretation: interpretation.to_string(),
        filter,
    }))
}

/// #14731 / #14963: resolve each declared gate against the placement this
/// process already resolved for this Cook, and fail admission only when a
/// gate is *known* to hard-fail there — before a provider is dispatched whose
/// work would be wasted on an unrecoverable verify gate.
///
/// This reuses the same preflight decision preview and dispatch both already
/// read (`parsed_command_preflight::captured_result`), so it costs no
/// additional live I/O and cannot disagree with what execution actually does.
///
/// A bare `homeboy review lint|test ...` gate (no explicit
/// `--placement`/`--runner`, or an explicit `--placement local` /
/// `lab-or-local`) is never rejected here, even under local placement with no
/// ready Lab runner: lint is not Lab-routed at all, and `review
/// test` either runs locally (the common case — resource admission only
/// engages under measured pressure) or gracefully **defers**. A deferred gate
/// is its own outcome, distinct from failed (#14731): the candidate stays
/// recoverable without re-running the provider, so rejecting it *before*
/// dispatch would refuse strictly more than the risk it guards against.
///
/// Only a gate that *pins* an unavailable route is rejected: `--placement
/// lab` (no local fallback) or an explicit `--runner <id>`. Both hard-fail at
/// execution time (`commands/infra/route.rs`) instead of deferring, so
/// admission fails closed on exactly that pinned, unrecoverable case.
pub(crate) fn reject_gates_unexecutable_under_resolved_placement(
    gates: impl IntoIterator<Item = String>,
) -> Result<()> {
    let Some(result) = homeboy::core::parsed_command_preflight::captured_result() else {
        return Ok(());
    };
    let selected_local =
        result.placement.selected == homeboy_lab_contract::EffectiveExecutionPlacement::Local;
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
    let Some((gate, requirement)) = gates
        .into_iter()
        .find_map(|command| gate_pins_unavailable_lab_route(&command).map(|req| (command, req)))
    else {
        return Ok(());
    };
    let (missing, hint) = match requirement {
        PinnedLabRouteRequirement::ExplicitLabPlacement => (
            "it declares `--placement lab`, which requests Lab with no local fallback".to_string(),
            "Connect a ready Lab runner before dispatching (see `homeboy runner status`), switch to `--placement lab-or-local` to permit a local fallback, or drop the flag so this gate can run locally.".to_string(),
        ),
        PinnedLabRouteRequirement::PinnedRunner(runner_id) => (
            format!("it pins `--runner {runner_id}`, which requires that specific runner to be ready"),
            format!(
                "Connect `{runner_id}` before dispatching (see `homeboy runner status`), or drop `--runner {runner_id}` so this gate can run locally."
            ),
        ),
    };
    Err(Error::validation_invalid_argument(
        "verify",
        format!(
            "declared gate `{gate}` cannot execute under the resolved local placement: {missing}"
        ),
        None,
        Some(vec![hint]),
    ))
}

/// A declared gate that hard-fails — rather than running locally or gracefully
/// deferring — because it pins a Lab route this process has no ready runner
/// for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PinnedLabRouteRequirement {
    ExplicitLabPlacement,
    PinnedRunner(String),
}

/// Only a `homeboy review test` gate that additionally pins an unavailable
/// Lab route is unrecoverable under local placement. `review lint` has
/// no Lab-routed defer path at all, and a bare/`auto`/`local`/
/// `lab-or-local` `review test` either runs locally or defers gracefully —
/// see `reject_gates_unexecutable_under_resolved_placement` for the full
/// reasoning.
fn gate_pins_unavailable_lab_route(command: &str) -> Option<PinnedLabRouteRequirement> {
    let argv = exact_simple_homeboy_invocation(command)?;
    if !(argv.len() >= 3 && argv[1] == "review" && argv[2] == "test") {
        return None;
    }
    if let Some(runner_id) = gate_flag_value(&argv, "--runner") {
        return Some(PinnedLabRouteRequirement::PinnedRunner(runner_id));
    }
    if gate_flag_value(&argv, "--placement").as_deref() == Some("lab") {
        return Some(PinnedLabRouteRequirement::ExplicitLabPlacement);
    }
    None
}

/// Read a `--flag value` or `--flag=value` occurrence from a parsed argv.
/// `--placement`/`--runner` are global clap flags (`conflicts_with` each
/// other), so they may appear anywhere in the declared gate's argv rather
/// than in a fixed position.
fn gate_flag_value(argv: &[String], flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    argv.iter().enumerate().find_map(|(index, token)| {
        if let Some(value) = token.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        if token == flag {
            return argv.get(index + 1).cloned();
        }
        None
    })
}

fn entry(command: String, kind: &'static str, status: &'static str) -> GateContractValidationEntry {
    GateContractValidationEntry {
        command,
        kind,
        status,
        mode: None,
        filter_interpretation: None,
        filter: None,
        selected_count: None,
        selected_ids: None,
        validation: None,
    }
}

fn exact_simple_homeboy_invocation(command: &str) -> Option<Vec<String>> {
    if command.contains(['|', '&', ';', '<', '>', '`', '$', '(', ')', '\n', '\r']) {
        return None;
    }
    let argv = shlex::split(command)?;
    (argv.first().map(String::as_str) == Some("homeboy")).then_some(argv)
}

pub(crate) fn declared_lab_gate_runner(command: &str) -> Result<Option<String>> {
    use clap::Parser;
    let Some(argv) = exact_simple_homeboy_invocation(command) else {
        return Ok(None);
    };
    let cli = crate::cli_surface::Cli::try_parse_from(argv)
        .map_err(|error| Error::invalid_argument("gate.declaration", error.to_string()))?;
    if cli.placement != crate::cli_surface::Placement::Lab && cli.runner.is_none() {
        return Ok(None);
    }
    let runner = cli.runner.or_else(|| {
        homeboy::core::parsed_command_preflight::captured_result().and_then(|result| {
            result
                .lab_readiness
                .and_then(|readiness| readiness.selected_runner_id)
        })
    });
    runner.map(Some).ok_or_else(|| {
        Error::invalid_argument(
            "gate.placement",
            "declared Lab gate has no admitted runner selection",
        )
    })
}

#[cfg(test)]
mod admitted_placement_tests {
    use super::*;

    #[test]
    fn explicit_lab_gate_uses_the_owned_parser_and_local_gates_stay_local() {
        assert_eq!(
            declared_lab_gate_runner("homeboy --runner lab review test --path .").unwrap(),
            Some("lab".to_string())
        );
        assert_eq!(
            declared_lab_gate_runner("homeboy --placement local review test --path .").unwrap(),
            None
        );
        assert_eq!(
            declared_lab_gate_runner("homeboy --placement lab review test --path . && printf next")
                .unwrap(),
            None
        );
    }
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
    fn accepts_builtin_extension_and_global_flag_commands() {
        let result = validate_gate_contracts(
            [
                "homeboy review lint --path .".to_string(),
                "homeboy extension-quality project".to_string(),
                "homeboy --placement local review lint".to_string(),
            ],
            None,
            &contract(),
        )
        .unwrap();
        assert_eq!(result.gates.len(), 3);
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
    fn rejects_missing_incomplete_and_version_skewed_commands() {
        for (gate, expected) in [
            ("homeboy lint component", "no `lint` command"),
            ("homeboy review missing", "no `review missing` command"),
            ("homeboy extension-quality", "incomplete"),
        ] {
            let error =
                validate_gate_contracts([gate.to_string()], None, &contract()).expect_err(gate);
            assert!(
                error.message.contains(expected),
                "{gate}: {}",
                error.message
            );
        }
    }

    /// External executables and compound shell are recorded unvalidated and
    /// never executed during admission.
    #[test]
    fn marks_external_and_compound_shell_gates_unvalidated() {
        for gate in [
            "missing-executable --would-run",
            "homeboy review lint && echo done",
        ] {
            let result = validate_gate_contracts([gate.to_string()], None, &contract()).unwrap();
            assert_eq!(result.gates[0].kind, "external", "{gate}");
            assert_eq!(result.gates[0].status, "unvalidated", "{gate}");
        }
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

    /// #14963 / #14731: admission only refuses a gate it can prove cannot run.
    /// A bare `review test` runs locally or defers recoverably; local-fallback
    /// placements and `review lint` have no Lab-only route; a ready Lab runner
    /// or a Lab-selected placement admits even a pinned gate.
    #[test]
    fn admits_gates_that_can_run_or_defer_under_the_resolved_placement() {
        for (gate, local_selected, lab_ready) in [
            ("homeboy review test homeboy", true, false),
            ("homeboy review test homeboy --placement local", true, false),
            (
                "homeboy review test homeboy --placement lab-or-local",
                true,
                false,
            ),
            ("homeboy review lint --path .", true, false),
            (
                "homeboy review lint data-machine --changed-since origin/main",
                true,
                false,
            ),
            ("homeboy review test homeboy --placement lab", true, true),
            ("homeboy review test homeboy --placement lab", false, false),
        ] {
            reject_gates_unexecutable_under_placement(
                [gate.to_string()],
                local_selected,
                lab_ready,
            )
            .unwrap_or_else(|_| panic!("`{gate}` ({local_selected}, {lab_ready}) is admitted"));
        }
    }

    /// #14731 / #14963: a `review test` gate pinned to Lab (`--placement lab`
    /// or `--runner`, either flag form) has no local fallback, so admission
    /// refuses it before a provider's work could be discarded.
    #[test]
    fn rejects_review_test_gates_pinned_to_an_unavailable_lab_route() {
        for (gate, pin) in [
            (
                "homeboy review test homeboy --placement lab",
                "--placement lab",
            ),
            (
                "homeboy review test homeboy --runner homeboy-lab",
                "--runner homeboy-lab",
            ),
            (
                "homeboy review test homeboy --runner=homeboy-lab",
                "homeboy-lab",
            ),
        ] {
            let error = reject_gates_unexecutable_under_placement([gate.to_string()], true, false)
                .expect_err(gate);
            assert!(error.message.contains(gate), "{}", error.message);
            assert!(error.message.contains(pin), "{}", error.message);
            assert!(
                error
                    .message
                    .contains("cannot execute under the resolved local placement"),
                "{}",
                error.message
            );
        }
    }

    #[test]
    fn preview_validates_cargo_shape_without_compiling_the_workspace() {
        let entries = validate_cargo_gate_contracts(
            ["cargo test job_store".to_string()],
            Some(Path::new("/workspace-without-a-cargo-manifest")),
        )
        .expect("module gate shape");
        assert_eq!(
            entries[0].filter_interpretation.as_deref(),
            Some("candidate_resolved")
        );
        assert_eq!(entries[0].status, "selection_deferred");
        assert_eq!(entries[0].selected_count, None);

        let exact = validate_cargo_gate_contracts(
            ["cargo test -p fixture selected_test -- --exact".to_string()],
            None,
        )
        .expect("exact gate shape");
        assert_eq!(exact[0].filter_interpretation.as_deref(), Some("exact"));
        assert_eq!(exact[0].status, "selection_deferred");
        assert_eq!(exact[0].selected_count, None);
        assert_eq!(
            exact[0].validation.as_deref(),
            Some("test_inventory_unavailable_without_build")
        );

        let nonexistent = validate_cargo_gate_contracts(
            ["cargo test -p fixture nonexistent::test_id -- --exact".to_string()],
            None,
        )
        .expect("without a cached test inventory, preview defers rather than claiming success");
        assert_eq!(nonexistent[0].status, "selection_deferred");
        assert_eq!(nonexistent[0].selected_count, None);
        assert_eq!(nonexistent[0].selected_ids, None);
    }

    #[test]
    fn preview_rejects_an_intrinsically_invalid_trailing_module_separator() {
        let error =
            validate_cargo_gate_contracts(["cargo test -p fixture git::".to_string()], None)
                .expect_err("trailing separator must fail before provider dispatch");
        assert!(error.message.contains("intrinsically invalid"));
        assert!(error.message.contains("git::"));
    }
}
