use super::*;

#[test]
fn fuzz_replay_parses_artifact_and_case_id_flags() {
    let cli = FuzzCli::parse_from([
        "fuzz",
        "replay",
        "/tmp/fuzz-results.json",
        "--component",
        "component-a",
        "--case-id",
        "case-1",
        "--run-id",
        "proof-1",
        "--",
        "--runner-flag",
    ]);

    match cli.args.command {
        Some(FuzzCommand::Replay(replay)) => {
            assert_eq!(replay.component.as_deref(), Some("component-a"));
            assert_eq!(
                replay.artifact_or_case.as_deref(),
                Some("/tmp/fuzz-results.json")
            );
            assert_eq!(replay.case_id.as_deref(), Some("case-1"));
            assert_eq!(replay.run_id.as_deref(), Some("proof-1"));
            assert_eq!(replay.args, vec!["--runner-flag"]);
        }
        _ => panic!("expected fuzz replay command"),
    }
}

#[test]
fn fuzz_minimize_parses_artifact_and_case_id_flags() {
    let cli = FuzzCli::parse_from([
        "fuzz",
        "minimize",
        "/tmp/fuzz-results.json",
        "--component",
        "component-a",
        "--case-id",
        "case-1",
        "--run-id",
        "proof-1",
        "--",
        "--runner-flag",
    ]);

    match cli.args.command {
        Some(FuzzCommand::Minimize(minimize)) => {
            assert_eq!(minimize.component.as_deref(), Some("component-a"));
            assert_eq!(
                minimize.artifact_or_case.as_deref(),
                Some("/tmp/fuzz-results.json")
            );
            assert_eq!(minimize.case_id.as_deref(), Some("case-1"));
            assert_eq!(minimize.run_id.as_deref(), Some("proof-1"));
            assert_eq!(minimize.args, vec!["--runner-flag"]);
        }
        _ => panic!("expected fuzz minimize command"),
    }
}

#[test]
fn fuzz_replay_resolves_campaign_metadata_without_executing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("fuzz-results.json");
    let campaign = serde_json::json!({
        "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
        "version": homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
        "id": "campaign-1",
        "safety_class": "read_only",
        "cases": [
            {
                "schema": homeboy::core::fuzz::FUZZ_CASE_SCHEMA,
                "id": "case-1",
                "replay_id": "replay-1"
            }
        ],
        "replay": {
            "schema": homeboy::core::fuzz::FUZZ_REPLAY_SCHEMA,
            "id": "replay-1",
            "seed": "1234",
            "artifact_id": "case-artifact"
        }
    });
    std::fs::write(&path, serde_json::to_string(&campaign).unwrap()).expect("write campaign");

    let output = run_replay(FuzzReplayArgs {
        component: None,
        path: None,
        rig: None,
        extension_override: ExtensionOverrideArgs::default(),
        setting_args: SettingArgs::default(),
        artifact_or_case: Some(path.to_string_lossy().to_string()),
        artifact: None,
        case_id: Some("case-1".to_string()),
        run_id: Some("proof-1".to_string()),
        dry_run: true,
        args: vec!["--runner-flag".to_string()],
    })
    .expect("resolve replay");
    let (output, exit) = output;

    assert_eq!(exit, 0);
    assert_eq!(output.status, "dry_run");
    assert_eq!(output.campaign_id.as_deref(), Some("campaign-1"));
    assert_eq!(output.case_id.as_deref(), Some("case-1"));
    assert_eq!(
        output.replay.as_ref().map(|replay| replay.id.as_str()),
        Some("replay-1")
    );
    assert!(output.env.iter().any(|env| {
        env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE" && env.value == path.to_string_lossy()
    }));
    assert!(output
        .env
        .iter()
        .any(|env| { env.name == "HOMEBOY_FUZZ_REPLAY_CASE_ID" && env.value == "case-1" }));
    assert!(output
        .env
        .iter()
        .any(|env| { env.name == "HOMEBOY_FUZZ_REPLAY_SEED" && env.value == "1234" }));
    assert_eq!(output.passthrough_args, vec!["--runner-flag"]);
}

#[test]
fn fuzz_replay_resolves_persisted_run_artifact_without_path() {
    with_isolated_home(|home| {
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                homeboy::core::observation::NewRunRecord::builder("fuzz")
                    .component_id("component-a")
                    .command("homeboy fuzz run component-a")
                    .cwd_path(home.path())
                    .build(),
            )
            .expect("run");
        let path = write_replay_campaign(home.path());
        store
            .record_artifact(&run.id, "fuzz_results", &path)
            .expect("artifact");

        let (output, exit) = run_replay(FuzzReplayArgs {
            component: None,
            path: None,
            rig: None,
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            artifact_or_case: None,
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some(run.id.clone()),
            dry_run: true,
            args: Vec::new(),
        })
        .expect("resolve run-backed replay");

        assert_eq!(exit, 0);
        assert_eq!(output.status, "dry_run");
        assert_eq!(output.campaign_id.as_deref(), Some("campaign-1"));
        assert_eq!(output.case_id.as_deref(), Some("case-1"));
        assert_eq!(output.run_id.as_deref(), Some(run.id.as_str()));
        assert!(output.artifact_file.as_deref().is_some_and(|artifact| {
            artifact.ends_with("fuzz-results.json") && artifact != path.to_string_lossy()
        }));
    });
}

#[test]
fn fuzz_replay_dry_run_resolves_persisted_homeboy_artifact_ref_without_local_bytes() {
    with_isolated_home(|home| {
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(
                homeboy::core::observation::NewRunRecord::builder("fuzz")
                    .component_id("component-a")
                    .command("homeboy fuzz run component-a")
                    .cwd_path(home.path())
                    .build(),
            )
            .expect("run");
        store
            .import_artifact(&homeboy::core::observation::ArtifactRecord {
                id: "artifact-1".to_string(),
                run_id: run.id.clone(),
                kind: "fuzz_result_envelope".to_string(),
                artifact_type: "file".to_string(),
                path: home
                    .path()
                    .join("missing-envelope.json")
                    .to_string_lossy()
                    .to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: None,
                size_bytes: None,
                mime: Some("application/json".to_string()),
                metadata_json: serde_json::json!({
                    "schema": homeboy::core::fuzz::FUZZ_RESULT_ENVELOPE_SCHEMA
                }),
                created_at: chrono::Utc::now().to_rfc3339(),
            })
            .expect("import artifact ref");

        let (output, exit) = run_replay(FuzzReplayArgs {
            component: None,
            path: None,
            rig: None,
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            artifact_or_case: None,
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some(run.id.clone()),
            dry_run: true,
            args: Vec::new(),
        })
        .expect("resolve homeboy artifact ref dry-run");

        assert_eq!(exit, 0);
        assert_eq!(output.status, "dry_run");
        let expected_ref = format!("homeboy://run/{}/artifact/artifact-1", run.id);
        assert_eq!(output.artifact_file.as_deref(), Some(expected_ref.as_str()));
        assert!(output.env.iter().any(|env| {
            env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE" && env.value == expected_ref
        }));
    });
}

#[test]
fn fuzz_replay_dry_run_accepts_runner_artifact_ref_without_local_bytes() {
    let reference = "runner-artifact://lab-runner/proof-run/fuzz-result-envelope";

    let (output, exit) = run_replay(FuzzReplayArgs {
        component: None,
        path: None,
        rig: None,
        extension_override: ExtensionOverrideArgs::default(),
        setting_args: SettingArgs::default(),
        artifact_or_case: Some(reference.to_string()),
        artifact: None,
        case_id: Some("case-1".to_string()),
        run_id: Some("proof-run".to_string()),
        dry_run: true,
        args: Vec::new(),
    })
    .expect("runner artifact dry-run");

    assert_eq!(exit, 0);
    assert_eq!(output.status, "dry_run");
    assert_eq!(output.artifact_file.as_deref(), Some(reference));
    assert_eq!(output.case_id.as_deref(), Some("case-1"));
    assert!(output
        .env
        .iter()
        .any(|env| { env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE" && env.value == reference }));
}

#[test]
fn fuzz_replay_runner_artifact_ref_without_dry_run_is_actionable() {
    let result = run_replay(FuzzReplayArgs {
        component: None,
        path: None,
        rig: None,
        extension_override: ExtensionOverrideArgs::default(),
        setting_args: SettingArgs::default(),
        artifact_or_case: Some(
            "runner-artifact://lab-runner/proof-run/fuzz-result-envelope".to_string(),
        ),
        artifact: None,
        case_id: Some("case-1".to_string()),
        run_id: Some("proof-run".to_string()),
        dry_run: false,
        args: Vec::new(),
    });
    let err = match result {
        Ok(_) => panic!("local bytes required for execution"),
        Err(err) => err,
    };

    assert_eq!(err.details["field"], "artifact");
    assert!(err.message.contains("requires local artifact bytes"));
    assert!(err.details["tried"]
        .as_array()
        .expect("tried entries")
        .iter()
        .any(|entry| entry
            .as_str()
            .is_some_and(|value| value.contains("--dry-run"))));
}

#[test]
fn fuzz_replay_executes_manifest_replay_command_with_env() {
    with_isolated_home(|home| {
        let component_dir = tempfile::tempdir().expect("component dir");
        write_fuzz_extension(
            home.path(),
            "fixture-fuzz",
            Some(
                r#"sh -c 'printf %s:%s:%s "$HOMEBOY_FUZZ_REPLAY_CASE_ID" "$HOMEBOY_FUZZ_REPLAY_SEED" "$1"' replay-runner {case}"#,
            ),
            None,
        );
        write_fuzz_rig(
            home,
            "fixture-rig",
            "component-a",
            component_dir.path(),
            "fixture-fuzz",
        );
        let path = write_replay_campaign(component_dir.path());

        let (output, exit) = run_replay(FuzzReplayArgs {
            component: Some("component-a".to_string()),
            path: None,
            rig: Some("fixture-rig".to_string()),
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            artifact_or_case: Some(path.to_string_lossy().to_string()),
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some("proof-1".to_string()),
            dry_run: false,
            args: vec!["--extra".to_string()],
        })
        .expect("execute replay");

        assert_eq!(exit, 0);
        assert_eq!(output.status, "passed");
        assert!(output.replay_command.as_deref().unwrap().contains("case-1"));
        assert!(output.env.iter().any(|env| {
            env.name == "HOMEBOY_FUZZ_REPLAY_ARTIFACT_FILE" && env.value == path.to_string_lossy()
        }));
        let execution = output.execution.expect("execution");
        assert_eq!(execution.extension_id, "fixture-fuzz");
        assert_eq!(execution.stdout, "case-1:1234:case-1");
    });
}

#[test]
fn fuzz_replay_reports_unsupported_when_manifest_has_no_replay_command() {
    with_isolated_home(|home| {
        let component_dir = tempfile::tempdir().expect("component dir");
        write_fuzz_extension(home.path(), "fixture-fuzz", None, None);
        write_fuzz_rig(
            home,
            "fixture-rig",
            "component-a",
            component_dir.path(),
            "fixture-fuzz",
        );
        let path = write_replay_campaign(component_dir.path());

        let (output, exit) = run_replay(FuzzReplayArgs {
            component: Some("component-a".to_string()),
            path: None,
            rig: Some("fixture-rig".to_string()),
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            artifact_or_case: Some(path.to_string_lossy().to_string()),
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some("proof-1".to_string()),
            dry_run: false,
            args: Vec::new(),
        })
        .expect("resolve unsupported replay");

        assert_eq!(exit, 1);
        assert_eq!(output.status, "unsupported");
        assert!(output
            .message
            .contains("does not declare fuzz.replay_command"));
        assert!(output.execution.is_none());
    });
}

#[test]
fn fuzz_minimize_executes_manifest_minimize_command_with_replay_env() {
    with_isolated_home(|home| {
        let component_dir = tempfile::tempdir().expect("component dir");
        write_fuzz_extension(
            home.path(),
            "fixture-fuzz",
            None,
            Some(
                r#"sh -c 'printf min:%s:%s:%s "$HOMEBOY_FUZZ_REPLAY_CASE_ID" "$HOMEBOY_FUZZ_REPLAY_SEED" "$1"' minimize-runner {case}"#,
            ),
        );
        write_fuzz_rig(
            home,
            "fixture-rig",
            "component-a",
            component_dir.path(),
            "fixture-fuzz",
        );
        let path = write_replay_campaign(component_dir.path());

        let (output, exit) = run_minimize(FuzzMinimizeArgs {
            component: Some("component-a".to_string()),
            path: None,
            rig: Some("fixture-rig".to_string()),
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            artifact_or_case: Some(path.to_string_lossy().to_string()),
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some("proof-1".to_string()),
            dry_run: false,
            args: vec!["--extra".to_string()],
        })
        .expect("execute minimize");

        assert_eq!(exit, 0);
        assert_eq!(output.command, "fuzz.minimize");
        assert_eq!(output.status, "passed");
        assert!(output.replay_command.as_deref().unwrap().contains("case-1"));
        let execution = output.execution.expect("execution");
        assert_eq!(execution.kind, "fuzz_minimize");
        assert_eq!(execution.stdout, "min:case-1:1234:case-1");
    });
}

#[test]
fn fuzz_minimize_reports_unsupported_without_minimize_command() {
    with_isolated_home(|home| {
        let component_dir = tempfile::tempdir().expect("component dir");
        write_fuzz_extension(home.path(), "fixture-fuzz", Some("true"), None);
        write_fuzz_rig(
            home,
            "fixture-rig",
            "component-a",
            component_dir.path(),
            "fixture-fuzz",
        );
        let path = write_replay_campaign(component_dir.path());

        let (output, exit) = run_minimize(FuzzMinimizeArgs {
            component: Some("component-a".to_string()),
            path: None,
            rig: Some("fixture-rig".to_string()),
            extension_override: ExtensionOverrideArgs::default(),
            setting_args: SettingArgs::default(),
            artifact_or_case: Some(path.to_string_lossy().to_string()),
            artifact: None,
            case_id: Some("case-1".to_string()),
            run_id: Some("proof-1".to_string()),
            dry_run: false,
            args: Vec::new(),
        })
        .expect("resolve unsupported minimize");

        assert_eq!(exit, 1);
        assert_eq!(output.command, "fuzz.minimize");
        assert_eq!(output.status, "unsupported");
        assert!(output
            .message
            .contains("does not declare fuzz.minimize_command"));
        assert!(output.execution.is_none());
    });
}

fn write_fuzz_extension(
    home: &Path,
    id: &str,
    replay_command: Option<&str>,
    minimize_command: Option<&str>,
) {
    let extension_dir = home.join(".config/homeboy/extensions").join(id);
    fs::create_dir_all(&extension_dir).expect("extension dir");
    let mut fuzz = serde_json::json!({
        "workloads": [{ "id": "replay-fixture" }]
    });
    if let Some(command) = replay_command {
        fuzz["replay_command"] = serde_json::Value::String(command.to_string());
    }
    if let Some(command) = minimize_command {
        fuzz["minimize_command"] = serde_json::Value::String(command.to_string());
    }
    fs::write(
        extension_dir.join(format!("{id}.json")),
        serde_json::json!({
            "name": id,
            "version": "0.0.0",
            "fuzz": fuzz
        })
        .to_string(),
    )
    .expect("write fuzz extension manifest");
}

fn write_fuzz_rig(
    home: &tempfile::TempDir,
    rig_id: &str,
    component_id: &str,
    path: &Path,
    extension_id: &str,
) {
    let rig_dir = home.path().join(".config/homeboy/rigs");
    fs::create_dir_all(&rig_dir).expect("rig dir");
    fs::write(
        rig_dir.join(format!("{rig_id}.json")),
        format!(
            r#"{{
                "components": {{
                    "{component_id}": {{
                        "path": "{}",
                        "extensions": {{ "{extension_id}": {{}} }}
                    }}
                }},
                "fuzz": {{ "default_component": "{component_id}" }}
            }}"#,
            path.display()
        ),
    )
    .expect("write fuzz rig");
}

fn write_replay_campaign(dir: &Path) -> PathBuf {
    let path = dir.join("fuzz-results.json");
    let campaign = serde_json::json!({
        "schema": homeboy::core::fuzz::FUZZ_CAMPAIGN_SCHEMA,
        "version": homeboy::core::fuzz::FUZZ_CONTRACT_VERSION,
        "id": "campaign-1",
        "safety_class": "read_only",
        "cases": [
            {
                "schema": homeboy::core::fuzz::FUZZ_CASE_SCHEMA,
                "id": "case-1",
                "replay_id": "replay-1"
            }
        ],
        "replay": {
            "schema": homeboy::core::fuzz::FUZZ_REPLAY_SCHEMA,
            "id": "replay-1",
            "seed": "1234",
            "artifact_id": "case-artifact"
        }
    });
    fs::write(&path, serde_json::to_string(&campaign).unwrap()).expect("write campaign");
    path
}
