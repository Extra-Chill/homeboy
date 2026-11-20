use super::*;

pub(super) fn gate_budget_failure_evidence(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    termination: AgentTaskGateTermination,
    progress_instrumented: bool,
    budget: Option<Duration>,
) -> Option<AgentTaskGateFailureEvidence> {
    let cause = match termination {
        AgentTaskGateTermination::TimedOut => format!(
            "wall-clock limit of {} ms exceeded; killed (timed_out)",
            budget.unwrap_or_default().as_millis()
        ),
        AgentTaskGateTermination::NoProgress => format!(
            "{} for {} ms; killed (no_progress)",
            if progress_instrumented {
                "no HOMEBOY_PROGRESS marker"
            } else {
                "no output"
            },
            budget.unwrap_or_default().as_millis()
        ),
        _ => return None,
    };
    Some(AgentTaskGateFailureEvidence {
        classification: AgentTaskGateFailureClassification::ExecutionBudget,
        summary: format!("deterministic gate supervision: {cause}: {command}"),
        command: command.to_string(),
        exit_code,
        stdout_tail: text_tail(stdout, 20),
        stderr_tail: text_tail(stderr, 20),
        failed_tests: Vec::new(),
        agent_feedback: "The supervisor stopped verification before a candidate verdict. Inspect the gate runtime and its declared time/progress budget, then resume verification. Do not rewrite candidate code based on this supervisor termination.".to_string(),
        diagnostics: Vec::new(),
    })
}

pub(crate) fn gate_failure_evidence(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    cargo_selection: Option<&AgentTaskGateCargoSelection>,
) -> AgentTaskGateFailureEvidence {
    let stdout_tail = text_tail(stdout, 20);
    let stderr_tail = text_tail(stderr, 20);
    let failed_tests = gate_failed_tests(stdout, stderr);
    let missing_script = npm_run_script(command).filter(|script| {
        stderr.contains(&format!("Missing script: \"{script}\""))
            || stderr.contains(&format!("Missing script: {script}"))
    });
    let selection_problem = cargo_selection.and_then(|selection| {
        selection
            .focused_problem()
            .map(|problem| (selection, problem))
    });
    let missing_executable = gate_missing_executable(command, exit_code, stdout, stderr);
    let shell_rejection = gate_shell_rejection(exit_code, stderr);
    let target_unresolved = homeboy_gate_target_unresolved(stdout, stderr);
    let setup_failure = homeboy_gate_setup_failure(exit_code, stdout, stderr);
    let environment_error = gate_environment_error(stdout, stderr);
    let classification = match selection_problem {
        _ if environment_error.is_some() || missing_executable.is_some() => {
            AgentTaskGateFailureClassification::Environment
        }
        Some((_, FocusedCargoSelectionProblem::NoTests)) => {
            AgentTaskGateFailureClassification::ZeroTestsSelected
        }
        None if missing_script.is_some()
            || missing_executable.is_some()
            || shell_rejection.is_some()
            || target_unresolved
            || setup_failure =>
        {
            AgentTaskGateFailureClassification::GateDeclaration
        }
        None => AgentTaskGateFailureClassification::CandidateCode,
    };
    let summary = if let Some(line) = environment_error {
        format!("gate environment failed: {line}")
    } else if let Some((selection, FocusedCargoSelectionProblem::NoTests)) = selection_problem {
        let filter = selection.filter.as_deref().unwrap_or_default();
        format!("invalid_focused_cargo_selection: filter {filter:?} selected no tests")
    } else if let Some(bin) = missing_executable.as_deref() {
        format!("gate environment cannot run `{bin}`: executable not found")
    } else {
        match (missing_script, shell_rejection.as_deref()) {
            (Some(script), _) => {
                format!("declared npm gate is missing script `{script}`: {command}")
            }
            (None, Some(line)) => {
                format!("gate shell `sh -c` rejected the gate program itself: {line}")
            }
            (None, None) if target_unresolved => format!(
                "gate environment cannot run `{command}`: Homeboy could not resolve the gate's target before checking the candidate"
            ),
            (None, None) if setup_failure => {
                "Homeboy gate setup failed before a candidate verdict; inspect the captured failure digest".to_string()
            }
            (None, None) => {
                format!("deterministic gate failed with exit code {exit_code}: {command}")
            }
        }
    };
    let agent_feedback = if environment_error.is_some() {
        "Environment failure, not candidate-code feedback. Preserve the candidate and re-gate after repairing the environment, or adopt locally.".to_string()
    } else if selection_problem.is_some() {
        cargo_selection
            .and_then(|selection| selection.recovery.as_ref())
            .map(|recovery| match recovery.action.as_str() {
                "rerun_exact" => format!(
                    "The Cargo test gate filter selected no tests. Rerun the structured exact recovery command `{}` before rerunning Cook.",
                    recovery.command
                ),
                _ => format!(
                    "The Cargo test gate filter selected no tests. Run the structured discovery command `{}` and choose a filter that selects at least one test before rerunning Cook.",
                    recovery.command
                ),
            })
            .unwrap_or_else(|| {
                "The Cargo test gate filter selected no tests. Choose a filter that selects at least one test before rerunning Cook.".to_string()
            })
    } else if let Some(bin) = missing_executable.as_deref() {
        format!("Environment failure, not candidate-code feedback. The gate environment cannot run `{bin}`: executable not found. Repair the environment and re-gate the preserved candidate, or adopt locally. Check dependency hydration at the gate's component path, including Composer dev dependencies in subdirectories. Do not change candidate code to satisfy this gate.")
    } else {
        match (missing_script, shell_rejection.as_deref()) {
            (Some(script), _) => format!(
                "The declared gate is invalid, not candidate-code feedback. Add `scripts.{script}` to the relevant package.json or change/remove `{command}` before rerunning Cook."
            ),
            (None, Some(line)) => format!(
                "The declared gate is invalid, not candidate-code feedback. Gates run under POSIX `sh -c`, which rejected the gate program before it checked anything (`{line}`). Rewrite it for POSIX sh (for example drop `set -o pipefail` and other bash-only syntax) or invoke bash explicitly, e.g. `bash ./verify.sh`, before rerunning Cook."
            ),
            (None, None) if target_unresolved => format!(
                "The gate is invalid, not candidate-code feedback. `{command}` could not resolve its target in the gate environment before it checked the candidate (for example the component is registered only in operator config that isolated gate XDG hides, no extension provides the capability, or an extension manifest could not be read). Declare a gate that runs from the repository itself (such as its own test script), or give the component a portable homeboy.json with the needed extension, then rerun Cook. Do not change repository code to satisfy this gate."
            ),
            (None, None) if setup_failure =>
                "Homeboy reported an infrastructure/setup error (exit 2), not a candidate verdict. Inspect the captured failure digest and repair the gate environment before resuming verification. Do not rewrite candidate code based on this failure.".to_string(),
            (None, None) if !failed_tests.is_empty() => format!(
                "A deterministic verification gate failed after the candidate patch was applied. Fix the code so `{command}` passes. These tests failed; use their messages and output as the primary failure evidence, then the captured stdout/stderr tails.\n{}",
                failed_tests_feedback(&failed_tests)
            ),
            (None, None) => format!(
                "A deterministic verification gate failed after the candidate patch was applied. Fix the code so `{command}` passes, using the captured stdout/stderr tails as the primary failure evidence."
            ),
        }
    };

    AgentTaskGateFailureEvidence {
        classification,
        summary,
        command: command.to_string(),
        exit_code,
        stdout_tail,
        stderr_tail,
        failed_tests,
        agent_feedback,
        diagnostics: Vec::new(),
    }
}

/// Failing tests named by the full captured gate output, read before the
/// 20-line tails are cut (#18130). The test command prints a failure digest
/// to stderr after the run, which survives the byte cap that a huge JSON
/// result (every executed test id) would otherwise fill; a complete JSON
/// result that still parses contributes its `failed_test_ids` as a fallback.
pub(super) fn gate_failed_tests(stdout: &str, stderr: &str) -> Vec<AgentTaskGateFailedTest> {
    use homeboy_core::extension::test::failure_digest::{
        parse_test_failure_digest, TEST_FAILURE_DIGEST_MAX_TESTS,
    };
    let digest = parse_test_failure_digest(&format!("{stderr}\n{stdout}"));
    if !digest.is_empty() {
        return digest;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return Vec::new();
    };
    let evidence = value
        .pointer("/data/test_runtime_evidence")
        .or_else(|| value.get("test_runtime_evidence"));
    evidence
        .and_then(|evidence| evidence.get("failed_test_ids"))
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .take(TEST_FAILURE_DIGEST_MAX_TESTS)
                .map(|id| AgentTaskGateFailedTest {
                    id: id.trim().to_string(),
                    file: String::new(),
                    message: String::new(),
                    output: String::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Bounded agent-facing listing of failed tests.
pub(super) fn failed_tests_feedback(failed_tests: &[AgentTaskGateFailedTest]) -> String {
    const MAX_BYTES: usize = 6 * 1024;
    let mut out = String::new();
    for test in failed_tests {
        let mut entry = format!("- `{}`", test.id);
        if !test.file.is_empty() {
            entry.push_str(&format!(" ({})", test.file));
        }
        if !test.message.is_empty() {
            entry.push_str(&format!(
                "\n  message: {}",
                test.message.replace('\n', "\n  ")
            ));
        }
        if !test.output.is_empty() {
            entry.push_str(&format!(
                "\n  output: {}",
                test.output.replace('\n', "\n  ")
            ));
        }
        if out.len() + entry.len() + 1 > MAX_BYTES {
            out.push_str("\n- … (more failed tests omitted)");
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&entry);
    }
    out
}

/// Recognize infrastructure diagnostics conservatively: a plain missing file
/// may be a candidate bug, but spawn failures and toolchain/setup errors are not.
/// Shared with feedback evaluation so older persisted candidate-code labels
/// cannot authorize another provider attempt for a broken environment.
pub(crate) fn gate_environment_error<'a>(stdout: &'a str, stderr: &'a str) -> Option<&'a str> {
    stderr.lines().chain(stdout.lines()).find(|line| {
        let text = line.to_ascii_lowercase();
        (text.contains("spawn")
            && (text.contains("enoent") || text.contains("no such file or directory")))
            || text.contains("no space left on device")
            || text.contains("enospc")
            || text.contains("inode exhaustion")
            || text.contains("disk quota exceeded")
            || (text.contains("toolchain")
                && (text.contains("not installed")
                    || text.contains("not found")
                    || text.contains("no default toolchain")))
            || text.contains("lab_workspace_dependency_hydration")
            || text.contains("runner.lab_transport_failure")
            || text.contains("gate_environment_unavailable")
            || ((text.contains("dependency hydration")
                || text.contains("workspace staging")
                || text.contains("gate transport"))
                && (text.contains("failed") || text.contains("error")))
    })
}

/// Missing PATH executables are environment failures, not candidate
/// failures. A missing path (`./bin/check`) stays candidate feedback: the
/// candidate may be the change that creates it.
pub(crate) fn gate_missing_executable(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> Option<String> {
    // Composer wraps a script's 127 in its own exit status (usually 1).
    // Require its explicit script diagnostic, not an arbitrary test's text.
    let composer_missing_command = exit_code != 0
        && stderr.lines().any(|line| {
            line.starts_with("Script ")
                && line.contains(" handling the ")
                && line.ends_with(" returned with error code 127")
        });
    if exit_code == 127 || composer_missing_command {
        for line in stderr.lines() {
            for suffix in [": command not found", ": not found"] {
                if let Some(prefix) = line.trim().strip_suffix(suffix) {
                    let bin = prefix
                        .rsplit(": ")
                        .next()
                        .unwrap_or_default()
                        .trim_matches(['\'', '"']);
                    if !bin.is_empty() && !bin.contains('/') {
                        return Some(bin.to_string());
                    }
                }
            }
        }
    }
    let pnpm_missing_command = exit_code == 254
        && [stdout, stderr]
            .iter()
            .any(|stream| stream.contains("ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL"));
    for stream in [stdout, stderr] {
        if pnpm_missing_command {
            if let Some(rest) = stream.split("Command \"").nth(1) {
                if let Some((bin, _)) = rest.split_once("\" not found") {
                    return Some(bin.to_string());
                }
            }
        }
        if stream.contains("could not determine executable to run") {
            let mut words = command.split_whitespace();
            let bin = words
                .find(|word| *word == "npx")
                .and_then(|_| words.find(|word| !word.starts_with('-')))
                .unwrap_or(command);
            return Some(bin.to_string());
        }
    }
    None
}

/// Detects a nested Homeboy gate (for example `homeboy review test
/// <component>`) that failed before checking the candidate.
///
/// The gate's stdout is a Homeboy command result. Two structured signals mean
/// the gate never reached the candidate: the `review_capability_preflight`
/// detail (an extension manifest unreadable in the gate environment,
/// Extra-Chill/homeboy#15381), or an error code for a target the gate
/// environment cannot resolve (`component.not_found` when the component is
/// registered only in the operator config that isolated gate XDG hides,
/// `extension.unsupported` when no extension provides the capability;
/// Extra-Chill/homeboy#16157). Either failure is identical on any candidate
/// and on the baseline, so it is a broken gate, never an inherited failure or
/// gate-fix feedback.
pub(super) fn homeboy_gate_target_unresolved(stdout: &str, stderr: &str) -> bool {
    [stdout, stderr].into_iter().any(|stream| {
        let Ok(result) = serde_json::from_str::<serde_json::Value>(stream.trim()) else {
            return false;
        };
        if result.get("success") != Some(&serde_json::Value::Bool(false)) {
            return false;
        }
        // Current results carry `diagnostics`; older ones carry `error`.
        let Some(error) = result.get("diagnostics").or_else(|| result.get("error")) else {
            return false;
        };
        error
            .get("details")
            .is_some_and(|details| details.get("review_capability_preflight").is_some())
            || error
                .get("code")
                .and_then(serde_json::Value::as_str)
                .and_then(homeboy_error::ErrorCode::from_str)
                .is_some_and(|code| code.is_unresolved_target())
    })
}

/// Exit 2 alone is not portable evidence of infrastructure failure (for
/// example, test runners use it too). Require Homeboy's structured error
/// envelope so an ordinary tool failure remains candidate feedback.
pub(super) fn homeboy_gate_setup_failure(exit_code: i32, stdout: &str, stderr: &str) -> bool {
    exit_code == 2
        && [stdout, stderr].into_iter().any(|stream| {
            let Ok(result) = serde_json::from_str::<serde_json::Value>(stream.trim()) else {
                return false;
            };
            result.get("success") == Some(&serde_json::Value::Bool(false))
                && result
                    .get("diagnostics")
                    .or_else(|| result.get("error"))
                    .and_then(|error| error.get("code"))
                    .and_then(serde_json::Value::as_str)
                    .is_some()
        })
}

/// Detects a gate program rejected by the POSIX sh interpreter itself.
///
/// Gates run under POSIX `sh` (dash on Debian/Ubuntu). A program using
/// bash-only syntax such as `set -o pipefail` dies before checking anything,
/// identically on the candidate and the immutable baseline. Without this,
/// the matching failures read as an inherited `baseline_red` environment
/// fault instead of a broken gate declaration (Extra-Chill/homeboy#15305).
///
/// Only interpreter-level diagnostics on exit status 2 qualify, the status
/// POSIX shells reserve for syntax and option errors. A plain `not found` is
/// handled separately by `gate_missing_executable`.
pub(super) fn gate_shell_rejection(exit_code: i32, stderr: &str) -> Option<String> {
    const MARKERS: [&str; 4] = [
        "Illegal option",
        "Syntax error",
        "syntax error",
        "Bad substitution",
    ];
    if exit_code != 2 {
        return None;
    }
    stderr
        .lines()
        .map(str::trim)
        .find(|line| {
            (line.starts_with("sh: ") || line.starts_with("/bin/sh: "))
                && MARKERS.iter().any(|marker| line.contains(marker))
        })
        .map(str::to_string)
}

/// A focused gate that selected no tests proves nothing, so it fails closed
/// even when cargo itself exits 0. Every other selection keeps cargo's real
/// exit code.
pub(super) fn effective_gate_exit_code(
    runner_exit_code: i32,
    cargo_selection: Option<&AgentTaskGateCargoSelection>,
) -> i32 {
    if cargo_selection.is_some_and(|selection| selection.focused_problem().is_some()) {
        1
    } else {
        runner_exit_code
    }
}

pub(super) fn cargo_selection(
    command: &str,
    effective_argv: &[String],
    stdout: &str,
    stderr: &str,
    test_result: Option<&AgentTaskGateTestResult>,
) -> Option<AgentTaskGateCargoSelection> {
    let tokens = crate::agent_task::tokenize_command(command);
    let cargo_index = cargo_test_index(&tokens)?;
    let args = &tokens[cargo_index + 1..];
    let test_index = args.iter().position(|token| token == "test")?;
    let harness = args.iter().position(|token| token == "--");
    let filter = cargo_test_filter(&args[test_index + 1..harness.unwrap_or(args.len())]);
    let exact =
        harness.is_some_and(|index| args[index + 1..].iter().any(|token| token == "--exact"));
    let mode = if filter.is_some() { "focused" } else { "broad" };
    let mut selected_ids = cargo_test_ids(stdout, stderr);
    if selected_ids.is_empty() && exact && test_result.is_some_and(|result| result.total == 1) {
        if let Some(filter) = &filter {
            selected_ids.push(filter.clone());
        }
    }
    let discovered_ids = {
        let listed = cargo_test_list_ids(stdout, stderr);
        if listed.is_empty() {
            selected_ids.clone()
        } else {
            listed
        }
    };
    // `cargo test -q` prints no per-test lines; its summaries still count
    // what the filter selected.
    let selected_count = match selected_ids.len() {
        0 => test_result.map_or(0, |result| result.total as usize),
        count => count,
    };
    let recovery = cargo_selection_recovery(
        &tokens,
        cargo_index,
        &discovered_ids,
        selected_count,
        exact,
        has_shell_control_operator(command),
    );
    Some(AgentTaskGateCargoSelection {
        effective_argv: effective_argv.to_vec(),
        mode: mode.to_string(),
        filter: filter.clone(),
        filter_interpretation: match (filter.is_some(), exact) {
            (false, _) => "broad_explicit".to_string(),
            (true, true) => "exact".to_string(),
            (true, false)
                if selected_count > 0
                    && filter.as_ref().is_some_and(|filter| {
                        selected_ids
                            .iter()
                            .all(|id| id.starts_with(&format!("{filter}::")))
                    }) =>
            {
                "module_prefix".to_string()
            }
            (true, false) => "substring_ambiguous".to_string(),
        },
        discovered_ids,
        selected_count,
        selected_ids,
        recovery,
    })
}

pub(super) fn cargo_selection_recovery(
    tokens: &[String],
    cargo_index: usize,
    discovered_ids: &[String],
    selected_count: usize,
    exact: bool,
    has_shell_control: bool,
) -> Option<AgentTaskGateCargoSelectionRecovery> {
    let test_index = tokens[cargo_index + 1..]
        .iter()
        .position(|token| token == "test")?
        + cargo_index
        + 1;
    let harness = tokens[test_index + 1..]
        .iter()
        .position(|token| token == "--")
        .map(|index| index + test_index + 1);
    let filter_index =
        cargo_test_filter_index(&tokens[test_index + 1..harness.unwrap_or(tokens.len())])
            .map(|index| index + test_index + 1);
    let focused = filter_index.is_some();
    let valid = focused && exact && selected_count == 1;
    if !focused || valid {
        return None;
    }

    let filter_index = filter_index?;
    let mut argv = tokens[..filter_index].to_vec();
    let action = if discovered_ids.len() == 1 && !has_shell_control {
        argv.push(discovered_ids[0].clone());
        "rerun_exact"
    } else {
        "discover_and_choose"
    };
    let before_harness_end = harness.unwrap_or(tokens.len());
    argv.extend_from_slice(&tokens[filter_index + 1..before_harness_end]);
    if let Some(harness) = harness {
        argv.push("--".to_string());
        argv.extend(
            tokens[harness + 1..]
                .iter()
                .filter(|argument| argument.as_str() != "--exact")
                .cloned(),
        );
    } else {
        argv.push("--".to_string());
    }
    argv.push(if action == "rerun_exact" {
        "--exact".to_string()
    } else {
        "--list".to_string()
    });
    let argv = redact_cargo_recovery_argv(argv);
    Some(AgentTaskGateCargoSelectionRecovery {
        action: action.to_string(),
        command: render_cargo_recovery_command(&argv),
        argv,
        candidate_ids: discovered_ids
            .iter()
            .take(MAX_CARGO_SELECTION_RECOVERY_CANDIDATES)
            .map(|id| homeboy_core::redaction::redact_string(id))
            .collect(),
    })
}

/// A tokenized shell command cannot faithfully retain control-flow syntax.
/// Recovery therefore declines an exact rerun whenever the declaration uses a
/// shell operator, while still exposing bounded discovery metadata.
pub(super) fn has_shell_control_operator(command: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if quote.is_none()
            && matches!(
                character,
                ';' | '|' | '&' | '(' | ')' | '{' | '}' | '<' | '>'
            )
        {
            return true;
        }
    }
    false
}

pub(super) fn render_cargo_recovery_command(argv: &[String]) -> String {
    let assignment_count = argv
        .iter()
        .take_while(|argument| is_environment_assignment(argument))
        .count();
    let mut rendered = argv[..assignment_count]
        .iter()
        .filter_map(|assignment| {
            let (name, value) = assignment.split_once('=')?;
            Some(format!(
                "{name}={}",
                homeboy_core::engine::shell::quote_arg(value)
            ))
        })
        .collect::<Vec<_>>();
    rendered.extend(
        argv[assignment_count..]
            .iter()
            .map(|argument| homeboy_core::engine::shell::quote_arg(argument)),
    );
    rendered.join(" ")
}

pub(super) fn redact_cargo_recovery_argv(argv: Vec<String>) -> Vec<String> {
    argv.into_iter()
        .map(|argument| homeboy_core::redaction::redact_string(&argument))
        .collect()
}

pub(super) fn cargo_test_filter(args: &[String]) -> Option<String> {
    cargo_test_filter_index(args).map(|index| args[index].clone())
}

pub(super) fn cargo_test_filter_index(args: &[String]) -> Option<usize> {
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        if !argument.starts_with('-') {
            return Some(index);
        }
        let takes_value = matches!(
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
        );
        index += 1 + usize::from(takes_value);
    }
    None
}

pub(super) fn cargo_test_ids(stdout: &str, stderr: &str) -> Vec<String> {
    let mut ids = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| line.trim().strip_prefix("test "))
        .filter_map(|line| line.split_once(" ... "))
        .filter_map(|(id, outcome)| matches!(outcome, "ok" | "FAILED" | "ignored").then_some(id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

pub(super) fn cargo_test_list_ids(stdout: &str, stderr: &str) -> Vec<String> {
    let mut ids = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| line.trim().strip_suffix(": test"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

pub(super) fn cargo_test_result(
    command: &str,
    runner_exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> Option<AgentTaskGateTestResult> {
    if !is_cargo_test_declaration(command) {
        return None;
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut filtered = 0;
    let mut found_summary = false;
    for line in stdout.lines().chain(stderr.lines()) {
        let Some((line_passed, line_failed, line_filtered)) = cargo_test_summary(line) else {
            continue;
        };
        found_summary = true;
        passed += line_passed;
        failed += line_failed;
        filtered += line_filtered;
    }
    found_summary.then_some(AgentTaskGateTestResult {
        runner: "cargo".to_string(),
        total: passed + failed,
        passed,
        failed,
        filtered,
        runner_exit_code,
    })
}

pub(super) fn is_cargo_test_declaration(command: &str) -> bool {
    let tokens = crate::agent_task::tokenize_command(command);
    cargo_test_index(&tokens).is_some()
}

pub(super) fn cargo_test_index(tokens: &[String]) -> Option<usize> {
    let mut index = 0;
    while tokens
        .get(index)
        .is_some_and(|token| is_environment_assignment(token))
    {
        index += 1;
    }
    let Some(first_command) = tokens.get(index) else {
        return None;
    };
    let cargo_index = if first_command == "cargo" {
        index
    } else if first_command == "timeout" {
        let Some(cargo_index) = timeout_cargo_index(&tokens, index + 1) else {
            return None;
        };
        cargo_index
    } else {
        return None;
    };
    is_cargo_test_subcommand(&tokens[cargo_index + 1..]).then_some(cargo_index)
}

pub(super) fn timeout_cargo_index(tokens: &[String], mut index: usize) -> Option<usize> {
    while tokens
        .get(index)
        .is_some_and(|token| token.starts_with('-'))
    {
        let takes_value = matches!(
            tokens[index].as_str(),
            "-k" | "--kill-after" | "-s" | "--signal"
        );
        index += 1 + usize::from(takes_value);
    }
    // `timeout` requires a duration immediately before the wrapped program.
    index += 1;
    tokens
        .get(index)
        .is_some_and(|token| token == "cargo")
        .then_some(index)
}

pub(super) fn is_environment_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
}

pub(super) fn is_cargo_test_subcommand(tokens: &[String]) -> bool {
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        if token == "test" {
            return true;
        }
        if token.starts_with('+') || token.starts_with('-') {
            let takes_value = matches!(token.as_str(), "--color" | "-C" | "--config" | "-Z");
            index += 1 + usize::from(takes_value);
            continue;
        }
        return false;
    }
    false
}

pub(super) fn cargo_test_summary(line: &str) -> Option<(u64, u64, u64)> {
    let line = line.trim();
    let rest = line.strip_prefix("test result: ok. ")?;
    let (passed, rest) = cargo_summary_count(rest, " passed; ")?;
    let (failed, rest) = cargo_summary_count(rest, " failed; ")?;
    let (_, rest) = cargo_summary_count(rest, " ignored; ")?;
    let (_, rest) = cargo_summary_count(rest, " measured; ")?;
    let (filtered, duration) = cargo_summary_count(rest, " filtered out; finished in ")?;
    let duration = duration.strip_suffix('s')?;
    (!duration.is_empty()
        && duration
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.'))
    .then_some((passed, failed, filtered))
}

pub(super) fn cargo_summary_count<'a>(text: &'a str, suffix: &str) -> Option<(u64, &'a str)> {
    let (count, rest) = text.split_once(suffix)?;
    Some((count.parse().ok()?, rest))
}

pub(crate) fn text_tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}
