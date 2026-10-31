use super::pi::*;
use crate::agent_task::AgentTaskOutcomeStatus;
use serde_json::json;

fn stream(message: serde_json::Value) -> String {
    format!(
        "{}\n{}\n",
        json!({"type":"message_end","message":message}),
        json!({"type":"turn_end"})
    )
}

#[test]
fn recorded_stream_outcomes() {
    let request = json!({"task_id":"pi-recording", "output_declarations":[{"name":"review_form","required":true}]});
    let success = include_str!("fixtures/pi-success.jsonl");
    assert_eq!(
        super::pi_response::served_models(std::io::Cursor::new(success)),
        vec!["upstream-model"]
    );
    let result = outcome(
        &request,
        &format!("invalid\n{success}"),
        "",
        Some(0),
        "pi",
        None,
    );
    assert_eq!(result.status, AgentTaskOutcomeStatus::Succeeded);
    assert_eq!(result.outputs["review_form"]["completion"], "complete");
    assert_eq!(result.metadata["usage"]["total_tokens"], 30.0);
    assert_eq!(result.metadata["usage"]["cost_usd"], 0.123457);
    assert_eq!(result.metadata["invalid_event_lines"], 1);
    assert_eq!(result.metadata["model"], "route");
    let error = stream(
        json!({"role":"assistant","stopReason":"error","errorMessage":"provider unavailable"}),
    );
    let result = outcome(&request, &error, "", Some(0), "pi", None);
    assert_eq!(result.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        result.summary.as_deref(),
        Some("Pi model request failed: provider unavailable")
    );
    let retry = format!(
        "{success}{}\n",
        json!({"type":"auto_retry_end","success":false,"finalError":"retry exhausted"})
    );
    let result = outcome(&request, &retry, "", Some(0), "pi", None);
    assert_eq!(result.status, AgentTaskOutcomeStatus::ProviderError);
    assert_eq!(
        result.summary.as_deref(),
        Some("Pi model request failed: retry exhausted")
    );
    let result = outcome(&request, "invalid\n", "", Some(0), "pi", None);
    assert_eq!(
        result.summary.as_deref(),
        Some("Pi exited without producing an assistant message.")
    );
    let result = outcome(&request, "", "1\n2\n3\n4\n5\n6", Some(1), "pi", None);
    assert_eq!(
        result.summary.as_deref(),
        Some("Pi execution failed: 2\n3\n4\n5\n6")
    );
    let result = outcome(
        &request,
        &stream(json!({"role":"assistant","stopReason":"stop"})),
        "",
        Some(0),
        "pi",
        None,
    );
    assert_eq!(result.status, AgentTaskOutcomeStatus::Failed);
}

#[test]
fn prompt_session_and_model_contract() {
    let temp = tempfile::tempdir().unwrap();
    let request = json!({"task_id":"a/b", "instructions":"é".repeat(60 * 1024), "executor":{"model":"roadie/test"}, "output_declarations":[{"name":"review_form","required":true}]});
    let args = args(&request, temp.path(), vec!["--mode".into(), "json".into()]).unwrap();
    assert_eq!(args[2], "--session");
    assert!(args[3].ends_with("a-b-pi-session.jsonl"));
    assert_eq!(&args[4..6], ["--model", "roadie/test"]);
    let instructions = std::fs::read_to_string(temp.path().join("a-b-pi-instructions.md")).unwrap();
    assert!(instructions.contains("Output declarations:"));
    assert!(instructions.contains("review_form"));
    assert!(args[6].starts_with("Your full task instructions are in "));
    assert_eq!(
        agent_dir(
            &json!({"pi_agent_dir":"~/custom"}),
            &[("HOME".into(), "/tmp/test-home".into())]
        ),
        "/tmp/test-home/custom"
    );
}

#[cfg(unix)]
#[test]
fn bounded_readiness_classifies_fake_pi() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let pi = temp.path().join("pi");
    std::fs::write(&pi, "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'pi 1.0'; else printf '%s\\n' \"$CATALOG\"; fi\n").unwrap();
    std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _env_lock = homeboy_core::test_support::env_lock();
    let path = std::env::join_paths(std::iter::once(temp.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let _path = homeboy_core::test_support::EnvVarGuard::set("PATH", path);
    let timeout = std::time::Duration::from_secs(2);
    for (model, catalog, reason) in [
        ("roadie/test", "roadie test", ""),
        ("test", "roadie test", "model_not_qualified"),
        ("other/test", "roadie test", "model_not_available"),
        ("roadie/test", "No models available", "no_models_available"),
    ] {
        let result = readiness(
            &json!({"model":model}),
            &[("CATALOG".into(), catalog.into())],
            timeout,
            None,
        )
        .unwrap();
        assert_eq!(result.reason, reason);
        assert_eq!(result.ready, reason.is_empty());
        assert!(!result.cache_key.is_empty());
        if reason == "model_not_qualified" {
            assert_eq!(result.identity["candidates"], json!(["roadie/test"]));
        }
    }
    let result = readiness(&json!({"command":pi}), &[], std::time::Duration::ZERO, None).unwrap();
    assert_eq!(result.classification, "transient_failure");
    assert!(result.retryable);
}
