use super::super::dispatch::raw_exec_command_run;
use super::super::exec::{
    exec, prepare_runner_exec_command, prepare_runner_exec_env, read_bounded,
    read_runner_exec_script, RUNNER_EXEC_SCRIPT_LIMIT_BYTES,
};
use super::super::types::RUNNER_EXEC_SCRIPT_ENV;

use homeboy::core::observation::{NewRunRecord, ObservationStore};
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

#[test]
fn runner_exec_promotes_declared_artifacts_to_run_store() {
    homeboy::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        runner::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                workspace.path().display()
            ),
            false,
        )
        .expect("create local runner");
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                NewRunRecord::builder("runner-exec")
                    .command("homeboy runner exec lab-local".to_string())
                    .cwd_path(workspace.path())
                    .metadata(serde_json::json!({}))
                    .build(),
            )
            .expect("run");

        let (_output, exit_code) = exec(
            "lab-local",
            Some(workspace.path().display().to_string()),
            None,
            false,
            false,
            Vec::new(),
            None,
            Vec::new(),
            false,
            Some(run.id.clone()),
            vec!["out.txt".to_string(), "reports".to_string()],
            Vec::new(),
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf hello > out.txt && mkdir reports && printf '{}' > reports/result.json"
                    .to_string(),
            ],
        )
        .expect("runner exec");

        assert_eq!(exit_code, 0);
        let artifacts = store.list_artifacts(&run.id).expect("artifacts");
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].kind, "out_txt");
        assert_eq!(artifacts[0].artifact_type, "file");
        assert!(std::path::Path::new(&artifacts[0].path).is_file());
        assert_eq!(artifacts[1].kind, "reports");
        assert_eq!(artifacts[1].artifact_type, "directory");
        assert!(std::path::Path::new(&artifacts[1].path)
            .join("result.json")
            .is_file());
    });
}

#[test]
fn runner_exec_promotes_declared_summaries_as_typed_evidence() {
    homeboy::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        runner::create(
            &format!(
                r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                workspace.path().display()
            ),
            false,
        )
        .expect("create local runner");
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                NewRunRecord::builder("runner-exec")
                    .command("homeboy runner exec lab-local".to_string())
                    .cwd_path(workspace.path())
                    .metadata(serde_json::json!({}))
                    .build(),
            )
            .expect("run");

        let (_output, exit_code) = exec(
            "lab-local",
            Some(workspace.path().display().to_string()),
            None,
            false,
            false,
            Vec::new(),
            None,
            Vec::new(),
            false,
            Some(run.id.clone()),
            Vec::new(),
            vec!["summary.json".to_string()],
            vec![
                "sh".to_string(),
                "-c".to_string(),
                r#"printf '{"matrix":{"passed":1}}' > summary.json"#.to_string(),
            ],
        )
        .expect("runner exec");

        assert_eq!(exit_code, 0);
        let artifacts = store.list_artifacts(&run.id).expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "summary");
        assert_eq!(artifacts[0].artifact_type, "file");
        assert_eq!(artifacts[0].metadata_json["declared_path"], "summary.json");
        assert_eq!(artifacts[0].metadata_json["evidence_role"], "summary");
        assert_eq!(artifacts[0].metadata_json["promoted_by"], "runner.exec");
        assert!(std::path::Path::new(&artifacts[0].path).is_file());
    });
}

#[test]
fn runner_exec_rejects_artifacts_without_run_id() {
    let err = exec(
        "lab-local",
        None,
        None,
        false,
        false,
        Vec::new(),
        None,
        Vec::new(),
        false,
        None,
        vec!["out.txt".to_string()],
        Vec::new(),
        vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
    )
    .expect_err("artifact requires run id");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert_eq!(err.details["field"], "run_id");
}

#[test]
fn runner_exec_rejects_summaries_without_run_id() {
    let err = exec(
        "lab-local",
        None,
        None,
        false,
        false,
        Vec::new(),
        None,
        Vec::new(),
        false,
        None,
        Vec::new(),
        vec!["summary.json".to_string()],
        vec!["sh".to_string(), "-c".to_string(), "printf ok".to_string()],
    )
    .expect_err("summary requires run id");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert_eq!(err.details["field"], "run_id");
}
