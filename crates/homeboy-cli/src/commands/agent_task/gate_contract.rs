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
    pub selected_count: Option<usize>,
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

/// Validate Cargo gate shape without executing Cargo. Test populations belong
/// to the candidate checkout, so compiling the base during preview would both
/// make preview unexpectedly expensive and reject tests the provider has not
/// created yet. Runtime gate evidence performs the bounded population check.
pub(crate) fn validate_cargo_gate_contracts(
    gates: impl IntoIterator<Item = String>,
    _workspace: Option<&Path>,
) -> Result<Vec<GateContractValidationEntry>> {
    let mut entries = Vec::new();
    for command in gates {
        let Some(selection) = cargo_gate_shape(&command)? else {
            continue;
        };
        entries.push(GateContractValidationEntry {
            command,
            kind: "cargo",
            status: "shape_valid",
            mode: Some(selection.mode),
            filter_interpretation: Some(selection.filter_interpretation),
            selected_count: None,
        });
    }
    Ok(entries)
}

struct CargoSelection {
    mode: String,
    filter_interpretation: String,
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
/// A bare `homeboy review lint|audit|test ...` gate (no explicit
/// `--placement`/`--runner`, or an explicit `--placement local` /
/// `lab-or-local`) is never rejected here, even under local placement with no
/// ready Lab runner: lint and audit are not Lab-routed at all, and `review
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
/// Lab route is unrecoverable under local placement. `review lint`/`review
/// audit` have no Lab-routed defer path at all, and a bare/`auto`/`local`/
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
        selected_count: None,
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

    /// #14963: a bare `review test` gate — Cook's own documented default
    /// gate (`homeboy --help` quick start) — must be admitted under local
    /// placement with no ready Lab runner. It either runs locally (the
    /// common case) or gracefully defers to a recoverable `Deferred` gate
    /// outcome (#14731); neither destroys the provider's work, so admission
    /// must not preemptively refuse it.
    #[test]
    fn admits_a_bare_review_test_gate_when_local_is_selected_and_no_lab_runner_is_ready() {
        reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy".to_string()],
            true,
            false,
        )
        .expect("a bare review test gate can run locally or defer gracefully");
    }

    /// `--placement local` and `--placement lab-or-local` both admit a local
    /// fallback, so neither hard-fails when Lab is unavailable.
    #[test]
    fn admits_a_review_test_gate_with_a_local_fallback_placement() {
        for placement in ["local", "lab-or-local"] {
            reject_gates_unexecutable_under_placement(
                [format!(
                    "homeboy review test homeboy --placement {placement}"
                )],
                true,
                false,
            )
            .unwrap_or_else(|_| panic!("--placement {placement} permits a local fallback"));
        }
    }

    /// #14731 / #14963: admission must still refuse a `review test` gate that
    /// *pins* an unavailable Lab route — `--placement lab` has no local
    /// fallback and hard-fails at execution time instead of deferring, so
    /// dispatching a provider first would still discard its work.
    #[test]
    fn rejects_a_review_test_gate_pinned_to_explicit_lab_placement_when_no_lab_runner_is_ready() {
        let error = reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy --placement lab".to_string()],
            true,
            false,
        )
        .expect_err("--placement lab has no local fallback and cannot execute locally");
        assert!(error
            .message
            .contains("homeboy review test homeboy --placement lab"));
        assert!(error
            .message
            .contains("cannot execute under the resolved local placement"));
        assert!(error.message.contains("--placement lab"));
    }

    /// An explicit `--runner <id>` pin is equally unrecoverable: it requires
    /// that specific runner, not just any ready Lab route.
    #[test]
    fn rejects_a_review_test_gate_pinned_to_an_explicit_runner_when_no_lab_runner_is_ready() {
        let error = reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy --runner homeboy-lab".to_string()],
            true,
            false,
        )
        .expect_err("a pinned --runner cannot execute locally");
        assert!(error.message.contains("--runner homeboy-lab"));
    }

    /// `--runner=<id>` (equals form) must be recognized identically to the
    /// space-separated form.
    #[test]
    fn recognizes_the_equals_form_of_a_pinned_runner_flag() {
        let error = reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy --runner=homeboy-lab".to_string()],
            true,
            false,
        )
        .expect_err("--runner=<id> is the same pin as the space-separated form");
        assert!(error.message.contains("homeboy-lab"));
    }

    #[test]
    fn admits_the_gate_when_a_lab_runner_is_ready() {
        reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy --placement lab".to_string()],
            true,
            true,
        )
        .expect("a ready Lab runner admits the same pinned gate");
    }

    #[test]
    fn admits_the_gate_when_placement_selected_lab() {
        reject_gates_unexecutable_under_placement(
            ["homeboy review test homeboy --placement lab".to_string()],
            false,
            false,
        )
        .expect("a Lab-selected placement admits the gate regardless of this local-only check");
    }

    /// #14963: `review lint` and `review audit` are not Lab-routed gates at
    /// all under this admission check — they must be admitted under local
    /// placement with no ready Lab runner just like a bare `review test`.
    #[test]
    fn admits_review_lint_and_review_audit_gates_under_local_placement() {
        for gate in [
            "homeboy review lint --path .",
            "homeboy review lint data-machine --changed-since origin/main",
            "homeboy review audit --path .",
            "homeboy review audit data-machine --changed-since origin/main",
        ] {
            reject_gates_unexecutable_under_placement([gate.to_string()], true, false)
                .unwrap_or_else(|_| panic!("`{gate}` has no Lab-routed defer path to reject"));
        }
    }

    #[test]
    fn does_not_classify_unrelated_gates_as_lab_routed() {
        reject_gates_unexecutable_under_placement(
            ["homeboy review lint --path .".to_string()],
            true,
            false,
        )
        .expect("only a pinned `homeboy review test` gate is rejected here");
    }

    #[test]
    fn preview_validates_cargo_shape_without_compiling_the_workspace() {
        let entries = validate_cargo_gate_contracts(
            ["cargo test generation_store".to_string()],
            Some(Path::new("/workspace-without-a-cargo-manifest")),
        )
        .expect("module gate shape");
        assert_eq!(
            entries[0].filter_interpretation.as_deref(),
            Some("candidate_resolved")
        );
        assert_eq!(entries[0].status, "shape_valid");
        assert_eq!(entries[0].selected_count, None);

        let exact = validate_cargo_gate_contracts(
            ["cargo test -p fixture selected_test -- --exact".to_string()],
            None,
        )
        .expect("exact gate shape");
        assert_eq!(exact[0].filter_interpretation.as_deref(), Some("exact"));
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
