//! Admission-time validation for deterministic gate command contracts.
//!
//! Shell gates remain opaque. This layer interprets only the explicitly owned
//! `homeboy` executable invocation against the installed command surface.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

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

/// Resolve Cargo gate shape against the already-resolved base workspace. This
/// is deliberately list-only: preview must spend no provider budget and must
/// not run the candidate gate, while still rejecting zero or ambiguous filters.
pub(crate) fn validate_cargo_gate_contracts(
    gates: impl IntoIterator<Item = String>,
    workspace: Option<&Path>,
) -> Result<Vec<GateContractValidationEntry>> {
    let Some(workspace) = workspace else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::new();
    for command in gates {
        let Some(selection) = cargo_gate_selection(&command, workspace)? else {
            continue;
        };
        if selection.selected_count == 0
            || (selection.filter.is_some()
                && !matches!(
                    selection.filter_interpretation.as_str(),
                    "exact" | "module_prefix"
                ))
        {
            return Err(Error::validation_invalid_argument(
                "verify",
                format!(
                    "Cargo gate `{command}` is invalid in preview: {} filter {:?} selected {} tests",
                    selection.filter_interpretation, selection.filter, selection.selected_count
                ),
                None,
                Some(vec![
                    "Use an exact test filter or a module prefix that selects one or more tests."
                        .to_string(),
                ]),
            ));
        }
        entries.push(GateContractValidationEntry {
            command,
            kind: "cargo",
            status: "selection_valid",
            mode: Some(selection.mode),
            filter_interpretation: Some(selection.filter_interpretation),
            selected_count: Some(selection.selected_count),
        });
    }
    Ok(entries)
}

struct CargoSelection {
    mode: String,
    filter: Option<String>,
    filter_interpretation: String,
    selected_count: usize,
}

fn cargo_gate_selection(command: &str, workspace: &Path) -> Result<Option<CargoSelection>> {
    let tokens = shlex::split(command).unwrap_or_default();
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
    let exact = harness.is_some_and(|index| args[index + 1..].iter().any(|arg| arg == "--exact"));
    let mut list_command = tokens[..cargo + 2].to_vec();
    list_command.extend_from_slice(args);
    if let Some(index) = list_command.iter().position(|arg| arg == "--") {
        list_command.truncate(index + 1);
        list_command.push("--list".to_string());
    } else {
        list_command.extend(["--".to_string(), "--list".to_string()]);
    }
    if exact {
        if let Some(index) = list_command.iter().position(|arg| arg == "--exact") {
            list_command.remove(index);
        }
    }
    let rendered = list_command
        .iter()
        .map(|arg| homeboy::core::engine::shell::quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let mut child = ProcessCommand::new("sh")
        .args(["-lc", &rendered])
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::internal_io(error.to_string(), Some(rendered.clone())))?;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child
            .try_wait()
            .map_err(|error| Error::internal_io(error.to_string(), Some(rendered.clone())))?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(Error::validation_invalid_argument(
                "verify",
                format!("Cargo gate preview timed out while listing tests: {command}"),
                None,
                None,
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child
        .wait_with_output()
        .map_err(|error| Error::internal_io(error.to_string(), Some(rendered)))?;
    let listing = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let selected_count = listing
        .lines()
        .filter(|line| line.trim_end().ends_with(": test"))
        .filter(|line| {
            filter.as_ref().is_none_or(|filter| {
                let id = line.trim_end().trim_end_matches(": test");
                exact && id == filter || !exact && id.contains(filter)
            })
        })
        .count();
    let interpretation = if filter.is_none() {
        "broad_explicit"
    } else if exact {
        "exact"
    } else if selected_count > 0
        && listing
            .lines()
            .filter_map(|line| line.trim_end().strip_suffix(": test"))
            .filter(|id| filter.as_ref().is_none_or(|filter| id.contains(filter)))
            .all(|id| {
                filter
                    .as_ref()
                    .is_some_and(|filter| id.starts_with(&format!("{filter}::")))
            })
    {
        "module_prefix"
    } else {
        "substring_ambiguous"
    };
    Ok(Some(CargoSelection {
        mode: if filter.is_some() { "focused" } else { "broad" }.to_string(),
        filter,
        filter_interpretation: interpretation.to_string(),
        selected_count,
    }))
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

    #[test]
    fn preview_resolves_module_prefix_and_rejects_zero_match() {
        let workspace = TempDir::new().expect("workspace");
        fs::create_dir(workspace.path().join("src")).expect("source directory");
        fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"preview-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("manifest");
        fs::write(
            workspace.path().join("src/lib.rs"),
            "#[cfg(test)] mod generation_store { #[test] fn first() {} #[test] fn second() {} }\n",
        )
        .expect("source");

        let entries = validate_cargo_gate_contracts(
            ["cargo test generation_store".to_string()],
            Some(workspace.path()),
        )
        .expect("module gate preview");
        assert_eq!(
            entries[0].filter_interpretation.as_deref(),
            Some("module_prefix")
        );
        assert_eq!(entries[0].selected_count, Some(2));

        let error = validate_cargo_gate_contracts(
            ["cargo test missing_module".to_string()],
            Some(workspace.path()),
        )
        .expect_err("zero-match gate must fail preview");
        assert!(error.message.contains("selected 0 tests"));
    }
}
