//! Post-upload install steps executed as one remote script (#16807).
//!
//! Clean, extract, flatten, layout check, ownership, both chmod walks, verify,
//! and cleanup used to be separate round trips. Each is now a fragment of one
//! generated script that delimits every step on both streams and reports the
//! failing step, so a failure keeps the step, exit code, and output it had as
//! its own remote command.

use homeboy_core::error::Result;
use homeboy_core::server::SshClient;

use super::types::DeployResult;

const STEP_MARKER: &str = "@@homeboy-step ";
const FAIL_MARKER: &str = "@@homeboy-fail ";
/// Lines a step prints with this prefix are relayed as deploy status lines.
pub(super) const LOG_MARKER: &str = "@@homeboy-log ";

/// Strip characters that would end or expand a double-quoted `echo` argument.
pub(super) fn shown(text: &str) -> String {
    text.replace(['"', '\\', '$', '`'], "")
}

/// What one step observed, as if it had run as its own remote command.
pub(super) struct StepOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl StepOutput {
    /// stderr, or stdout when the step wrote nothing to stderr.
    pub fn detail(&self) -> &str {
        if self.stderr.is_empty() {
            &self.stdout
        } else {
            &self.stderr
        }
    }
}

type FailureHandler = Box<dyn Fn(&StepOutput) -> Result<DeployResult>>;

pub(super) struct Step {
    command: String,
    /// `None` ignores a non-zero exit and continues.
    on_failure: Option<FailureHandler>,
}

impl Step {
    pub fn new(
        command: impl Into<String>,
        on_failure: impl Fn(&StepOutput) -> Result<DeployResult> + 'static,
    ) -> Self {
        let on_failure: FailureHandler = Box::new(on_failure);
        Self {
            command: command.into(),
            on_failure: Some(on_failure),
        }
    }

    pub fn best_effort(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            on_failure: None,
        }
    }

    /// Succeeds only when `command` exits 0 and prints something other than
    /// whitespace — the deploy verification contract.
    pub fn requiring_output(
        command: &str,
        on_failure: impl Fn(&StepOutput) -> Result<DeployResult> + 'static,
    ) -> Self {
        Self::new(
            format!(
                "out=$(mktemp) || exit 1\n( {command}\n) >\"$out\"; rc=$?; cat \"$out\"\n\
                 [ \"$rc\" -eq 0 ] && grep -q '[^[:space:]]' \"$out\"; rc=$?; rm -f \"$out\"; exit \"$rc\""
            ),
            on_failure,
        )
    }
}

/// Run every step in one remote invocation. Returns `Ok(None)` when every
/// aborting step succeeded, or the failing step's mapped result.
pub(super) fn run(ssh_client: &SshClient, steps: &[Step]) -> Result<Option<DeployResult>> {
    if steps.is_empty() {
        return Ok(None);
    }
    let mut script = String::new();
    for (index, step) in steps.iter().enumerate() {
        // A subshell scopes each step's `cd`, `exit`, and EXIT traps to the
        // step, exactly as when it ran as its own command.
        script.push_str(&format!(
            "printf '\\n{STEP_MARKER}{index}\\n'; printf '\\n{STEP_MARKER}{index}\\n' >&2\n( {}\n); rc=$?\n",
            step.command
        ));
        if step.on_failure.is_some() {
            script.push_str(&format!(
                "if [ \"$rc\" -ne 0 ]; then printf '\\n{FAIL_MARKER}{index} %s\\n' \"$rc\"; exit \"$rc\"; fi\n"
            ));
        }
    }
    let output = ssh_client.execute(&script);
    for line in output.stdout.lines() {
        if let Some(message) = line.strip_prefix(LOG_MARKER) {
            homeboy_core::log_status!("deploy", "{}", message);
        }
    }
    if output.success {
        return Ok(None);
    }

    // A failing step reports itself. Without a report the script never
    // finished (e.g. the transport dropped): attribute that to the last step
    // that started, with the transport's exit code.
    let report = output.stdout.lines().find_map(|line| {
        let (index, code) = line.strip_prefix(FAIL_MARKER)?.split_once(' ')?;
        Some((
            index.parse::<usize>().ok()?,
            code.trim().parse::<i32>().ok()?,
        ))
    });
    let (index, exit_code) = report.unwrap_or_else(|| {
        let started = output.stdout.lines().filter(|l| l.starts_with(STEP_MARKER));
        (started.count().saturating_sub(1), output.exit_code)
    });
    let index = index.min(steps.len() - 1);
    let step_output = StepOutput {
        exit_code,
        stdout: step_segment(&output.stdout, index)
            .split_inclusive('\n')
            .filter(|line| !line.starts_with(LOG_MARKER))
            .collect(),
        stderr: step_segment(&output.stderr, index),
    };
    match &steps[index].on_failure {
        Some(handler) => handler(&step_output).map(Some),
        None => Ok(Some(DeployResult::failure(
            exit_code,
            step_output.detail().to_string(),
        ))),
    }
}

/// One step's share of a stream. Markers print as `\n<marker>\n`, so the
/// newline ahead of the next marker belongs to the marker, not the step.
fn step_segment(stream: &str, index: usize) -> String {
    let start = format!("\n{STEP_MARKER}{index}\n");
    let Some(begin) = stream.find(&start).map(|at| at + start.len()) else {
        return String::new();
    };
    let rest = &stream[begin..];
    let end = [format!("\n{STEP_MARKER}"), format!("\n{FAIL_MARKER}")]
        .iter()
        .filter_map(|marker| rest.find(marker.as_str()))
        .min()
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::local_client;

    #[test]
    fn reports_the_failing_step_with_its_own_exit_code_and_output() {
        let steps = vec![
            Step::new("echo first; echo first-err >&2", |_| unreachable!()),
            Step::best_effort("exit 9"),
            Step::new("printf partial; echo boom >&2; exit 4", |output| {
                let detail = format!("[{}][{}]", output.stdout, output.stderr);
                Ok(DeployResult::failure(output.exit_code, detail))
            }),
            Step::new("echo never-runs", |_| unreachable!()),
        ];

        let result = run(&local_client(), &steps).expect("run").expect("failure");

        assert_eq!(result.exit_code, 4);
        assert_eq!(result.error.as_deref(), Some("[partial][boom\n]"));
    }

    #[test]
    fn output_requiring_step_fails_on_blank_output_or_error_exit() {
        let fail = |_: &StepOutput| Ok(DeployResult::failure(1, "verify".to_string()));
        for (command, passes) in [
            ("printf ' \\n'", false),
            ("printf ok; exit 1", false),
            ("case x in x) printf ok ;; esac", true),
        ] {
            let outcome = run(&local_client(), &[Step::requiring_output(command, fail)]);
            assert_eq!(outcome.expect("run").is_none(), passes, "{command}");
        }
    }
}
