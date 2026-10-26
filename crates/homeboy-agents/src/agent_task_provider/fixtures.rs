use super::command_runner::failure_outcome;
use super::*;

pub(super) fn run_fixture_provider(
    request: &AgentTaskRequest,
    artifact_root: &Path,
) -> AgentTaskOutcome {
    if let Some(path) = std::env::var_os("HOMEBOY_FIXTURE_PROVIDER_EXECUTIONS_FILE") {
        use std::io::Write;
        let mut ledger = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open fixture execution ledger");
        writeln!(ledger, "{}", artifact_root.display()).expect("count fixture execution");
    }
    if let Some(path) = std::env::var_os("HOMEBOY_FIXTURE_PROVIDER_STARTED_FILE") {
        let _ = std::fs::write(path, "started\n");
    }
    // A test can hold actual provider work until its observing client exits,
    // without assuming the client can inspect and terminate within a delay.
    #[cfg(unix)]
    if let Some(path) = std::env::var_os("HOMEBOY_FIXTURE_PROVIDER_RELEASE_SOCKET") {
        use std::io::Read;
        let mut stream = std::os::unix::net::UnixStream::connect(path)
            .expect("connect fixture provider release socket");
        let mut release = [0];
        stream
            .read_exact(&mut release)
            .expect("release fixture provider");
        assert_eq!(release, [1], "fixture provider release token");
    }
    if let Some(delay_ms) = std::env::var("HOMEBOY_FIXTURE_PROVIDER_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
    let mode = request
        .executor
        .config
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("success");
    match mode {
        "empty_patch" => fixture_empty_patch_outcome(request, artifact_root),
        "empty_runtime_bundle" => fixture_empty_runtime_bundle_outcome(request, artifact_root),
        _ => fixture_success_outcome(request, artifact_root),
    }
}

fn fixture_success_outcome(request: &AgentTaskRequest, artifact_root: &Path) -> AgentTaskOutcome {
    let changed_file = request
        .executor
        .config
        .get("changed_file")
        .and_then(Value::as_str)
        .unwrap_or("docs/agent-task-smoke.md");
    let transcript_path = artifact_root.join("transcript.log");
    let result_path = artifact_root.join("agent-result.json");
    // Like a real provider, edit the attempt workspace; Homeboy's harvest
    // turns the edited tree into the candidate commit (#16303).
    if let Some(root) = request.workspace.root.as_deref() {
        let path = Path::new(root).join(changed_file);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, "after\n");
    }
    let _ = std::fs::write(
        &transcript_path,
        "fixture provider completed successfully\n",
    );

    let metadata = request
        .executor
        .config
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({ "fixture_mode": "success" }));
    let mut outcome = AgentTaskOutcome {
        task_id: request.task_id.clone(),
        status: AgentTaskOutcomeStatus::Succeeded,
        summary: Some("fixture provider wrote deterministic smoke artifacts".to_string()),
        artifacts: vec![fixture_artifact(
            "agent-result",
            "agent_result",
            &result_path,
            Some("application/json"),
        )],
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: transcript_path.display().to_string(),
            label: Some("fixture transcript".to_string()),
        }],
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.fixture_success".to_string(),
            message:
                "fixture provider edited the workspace and wrote transcript and result artifacts"
                    .to_string(),
            data: json!({ "artifact_root": artifact_root.display().to_string() }),
        }],
        metadata,
        ..Default::default()
    };
    let _ = std::fs::write(
        &result_path,
        serde_json::to_string_pretty(&outcome).unwrap_or_else(|_| "{}".to_string()),
    );
    outcome.artifacts[0] = fixture_artifact(
        "agent-result",
        "agent_result",
        &result_path,
        Some("application/json"),
    );
    outcome
}

fn fixture_empty_patch_outcome(
    request: &AgentTaskRequest,
    artifact_root: &Path,
) -> AgentTaskOutcome {
    let patch_path = artifact_root.join("empty.patch");
    let _ = std::fs::write(&patch_path, "");
    failure_outcome(
        request,
        AgentTaskOutcomeStatus::NoOp,
        AgentTaskFailureClassification::InvalidInput,
        "agent_task.fixture_empty_patch",
        "fixture produced an empty patch; promotion should reject it".to_string(),
        json!({ "patch_path": patch_path.display().to_string() }),
    )
}

fn fixture_empty_runtime_bundle_outcome(
    request: &AgentTaskRequest,
    artifact_root: &Path,
) -> AgentTaskOutcome {
    let bundle_path = artifact_root.join("runtime-bundle");
    let _ = std::fs::create_dir_all(&bundle_path);
    let mut outcome = failure_outcome(
        request,
        AgentTaskOutcomeStatus::ProviderError,
        AgentTaskFailureClassification::Provider,
        "agent_task.fixture_empty_runtime_bundle",
        "fixture produced an empty runtime bundle".to_string(),
        json!({ "bundle_path": bundle_path.display().to_string() }),
    );
    outcome.artifacts.push(fixture_artifact(
        "runtime-bundle",
        "runtime_bundle",
        &bundle_path,
        None,
    ));
    outcome
}

pub(super) fn fixture_artifact(
    id: &str,
    kind: &str,
    path: &PathBuf,
    mime: Option<&str>,
) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.to_string(),
        kind: kind.to_string(),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().to_string()),
        label: None,
        role: None,
        semantic_key: None,
        path: Some(path.display().to_string()),
        url: None,
        mime: mime.map(str::to_string),
        size_bytes: std::fs::metadata(path).ok().map(|metadata| metadata.len()),
        sha256: None,
        metadata: json!({ "fixture": true }),
    }
}
