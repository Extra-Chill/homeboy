use super::super::dispatch::raw_exec_command_run;
use super::super::exec::{
    prepare_runner_exec_command, prepare_runner_exec_env, read_bounded, read_runner_exec_script,
    RUNNER_EXEC_SCRIPT_LIMIT_BYTES,
};
use super::super::types::RUNNER_EXEC_SCRIPT_ENV;

use homeboy::core::runners::{self as runner, RunnerExecOutput};

#[test]
fn raw_exec_command_run_keeps_structured_output_and_presentation_streams() {
    let run = raw_exec_command_run(
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: runner::RunnerExecMode::Daemon,
            argv: vec!["printf".to_string(), "hello".to_string()],
            remote_cwd: "/workspace".to_string(),
            exit_code: 7,
            stdout: "hello\n".to_string(),
            stderr: "warn\n".to_string(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: Some("job-123".to_string()),
            job_events: None,
            mirror_run_id: None,
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            metrics: None,
            capture: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        },
        7,
    );

    assert_eq!(run.exit_code, 7);
    assert_eq!(run.presentation.stdout.as_deref(), Some("hello\n"));
    assert_eq!(run.presentation.stderr.as_deref(), Some("warn\n"));

    let value = run.stdout_result.expect("structured output");
    assert_eq!(value["command"], "runner.exec");
    assert_eq!(value["variant"], "exec");
    assert_eq!(value["stdout"], "hello\n");
    assert_eq!(value["stderr"], "warn\n");
    assert_eq!(value["job_id"], "job-123");
}

#[test]
fn read_bounded_retains_full_source_within_limit() {
    let (bytes, capture) = read_bounded(&b"echo hi"[..], 1024).expect("read bounded");

    assert_eq!(bytes, b"echo hi");
    assert_eq!(capture.limit_bytes, 1024);
    assert_eq!(capture.seen_bytes, 7);
    assert_eq!(capture.retained_bytes, 7);
    assert!(!capture.truncated);
}

#[test]
fn read_bounded_marks_truncated_when_source_exceeds_limit() {
    let source = [b'x'; 16];
    let (bytes, capture) = read_bounded(&source[..], 4).expect("read bounded");

    assert_eq!(bytes.len(), 4);
    assert_eq!(capture.limit_bytes, 4);
    assert_eq!(capture.retained_bytes, 4);
    assert!(capture.seen_bytes > capture.retained_bytes);
    assert!(capture.truncated);
}

#[test]
fn read_runner_exec_script_rejects_oversized_script() {
    use std::io::Write;

    let mut file = tempfile::NamedTempFile::new().expect("temp script");
    let oversized = vec![b'a'; RUNNER_EXEC_SCRIPT_LIMIT_BYTES + 1];
    file.write_all(&oversized).expect("write script");
    let path = file.path().to_string_lossy().to_string();

    let err = read_runner_exec_script(&path).expect_err("oversized script rejected");
    assert!(err.to_string().contains("byte limit"));
}

#[test]
fn script_file_prepares_bash_stdin_command() {
    let command = prepare_runner_exec_command(Some(&"echo hi".to_string()), Vec::new())
        .expect("script command");

    assert_eq!(command[0], "bash");
    assert_eq!(command[1], "-c");
    assert!(command[2].contains(RUNNER_EXEC_SCRIPT_ENV));
}

#[test]
fn script_file_rejects_extra_argv() {
    let err = prepare_runner_exec_command(Some(&"echo hi".to_string()), vec!["printf".to_string()])
        .expect_err("script plus argv should fail");

    assert!(err
        .to_string()
        .contains("either --script-file or a command"));
}

#[test]
fn env_parser_injects_script_body_without_shell_quoting() {
    let env = prepare_runner_exec_env(
        vec!["GREETING=hello world".to_string()],
        Some("echo \"$GREETING\""),
    )
    .expect("env");

    assert_eq!(env["GREETING"], "hello world");
    assert_eq!(env[RUNNER_EXEC_SCRIPT_ENV], "echo \"$GREETING\"");
}
