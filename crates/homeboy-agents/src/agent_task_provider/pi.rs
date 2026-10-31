//! Pi's native CLI contract. Process supervision remains in command_runner.
use std::path::Path;

use serde_json::{json, Value};

use super::command_runner::ProviderReadinessInvocationResult;
use crate::agent_task::{
    AgentTaskDiagnostic, AgentTaskFailureClassification, AgentTaskOutcome, AgentTaskOutcomeStatus,
};

const OUTPUT_INSTRUCTIONS: &str = include_str!("pi-outputs.prompt.md");

pub(super) fn command(config: &Value) -> Result<(String, Vec<String>), String> {
    let program = config
        .get("command")
        .cloned()
        .or_else(|| std::env::var("HOMEBOY_PI_COMMAND").ok().map(Value::String))
        .unwrap_or(json!("pi"));
    let program = program
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or("executor.config.command must be a non-empty string when provided.")?
        .trim()
        .to_string();
    let args = config
        .get("command_args")
        .cloned()
        .or_else(|| {
            std::env::var("HOMEBOY_PI_COMMAND_ARGS")
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .filter(Value::is_array)
        })
        .unwrap_or(json!(["--mode", "json"]));
    let args = args
        .as_array()
        .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
        .ok_or("executor.config.command_args must be an array of strings when provided.")?;
    Ok((program, args))
}

pub(super) fn agent_dir(config: &Value, env: &[(String, String)]) -> String {
    let lookup = |name: &str| {
        env.iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(name).ok())
    };
    let home = lookup("HOME").unwrap_or_default();
    if let Some(dir) = config["pi_agent_dir"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if dir == "~" {
            return home;
        }
        if let Some(suffix) = dir.strip_prefix("~/") {
            return Path::new(&home).join(suffix).display().to_string();
        }
        return dir.to_string();
    }
    lookup("PI_CODING_AGENT_DIR")
        .unwrap_or_else(|| Path::new(&home).join(".pi/agent").display().to_string())
}

fn safe_segment(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || "._-".contains(ch) {
            result.push(ch);
        } else if !result.ends_with('-') {
            result.push('-');
        }
    }
    let result = result.trim_matches('-');
    if result.is_empty() {
        "agent-task".into()
    } else {
        result.into()
    }
}

fn declarations(request: &Value) -> Vec<Value> {
    let direct = request["output_declarations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut declarations = request["inputs"]["required_outputs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    declarations.retain(|d| !direct.iter().any(|other| other["name"] == d["name"]));
    declarations.extend(direct);
    declarations.retain(|d| d["name"].as_str().is_some_and(|s| !s.trim().is_empty()));
    for declaration in &mut declarations {
        declaration["name"] = json!(declaration["name"].as_str().unwrap().trim());
        declaration["required"] = json!(declaration["required"] == true);
        if declaration.get("json_schema").is_none() {
            if let Some(schema) = declaration.get("structural_schema").cloned() {
                declaration["json_schema"] = schema;
            }
        }
        declaration
            .as_object_mut()
            .unwrap()
            .remove("structural_schema");
    }
    declarations
}

pub(super) fn args(
    request: &Value,
    artifacts: &Path,
    mut args: Vec<String>,
) -> Result<Vec<String>, String> {
    let config = &request["executor"]["config"];
    let task = safe_segment(request["task_id"].as_str().unwrap_or_default());
    if artifacts.as_os_str().is_empty() {
        args.push("--no-session".into());
    } else {
        std::fs::create_dir_all(artifacts).map_err(|e| e.to_string())?;
        args.extend([
            "--session".into(),
            artifacts
                .join(format!("{task}-pi-session.jsonl"))
                .display()
                .to_string(),
        ]);
    }
    if let Some(model) = config["model"]
        .as_str()
        .or(request["executor"]["model"].as_str())
        .or(request["model"].as_str())
        .filter(|s| !s.is_empty())
    {
        args.extend(["--model".into(), model.into()]);
    }
    let mut prompt = request["instructions"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let declared = declarations(request);
    if !declared.is_empty() {
        let suffix = config["output_instructions_template"]
            .as_str()
            .unwrap_or(OUTPUT_INSTRUCTIONS)
            .replace(
                "{{declarations}}",
                &serde_json::to_string(&declared).unwrap(),
            );
        if !suffix.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(suffix.trim());
        }
    }
    if prompt.len() > 100 * 1024 {
        let dir = if artifacts.as_os_str().is_empty() {
            std::env::temp_dir().join(format!("homeboy-pi-{}", uuid::Uuid::new_v4()))
        } else {
            artifacts.to_path_buf()
        };
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("{task}-pi-instructions.md"));
        std::fs::write(&file, prompt).map_err(|e| e.to_string())?;
        prompt = format!("Your full task instructions are in {}. Read that file first, then carry out the instructions exactly.", file.display());
    }
    args.push(prompt);
    Ok(args)
}

fn structured_answer(text: &str) -> Option<Value> {
    let mut candidates = vec![text.trim()];
    let mut rest = text;
    while let Some((_, after)) = rest.split_once("```") {
        let Some((block, after)) = after.split_once("```") else {
            break;
        };
        candidates.insert(
            0,
            block
                .strip_prefix("json")
                .or_else(|| block.strip_prefix("JSON"))
                .unwrap_or(block)
                .trim(),
        );
        rest = after;
    }
    candidates
        .into_iter()
        .filter(|s| s.len() <= 80 * 1024)
        .find_map(|s| {
            serde_json::from_str::<Value>(s)
                .ok()
                .filter(Value::is_object)
        })
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub(super) fn outcome(
    request: &Value,
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    program: &str,
    cwd: Option<&Path>,
) -> AgentTaskOutcome {
    let mut events = 0;
    let mut invalid = 0;
    let mut turns = 0;
    let mut tools = 0;
    let mut tool_errors = 0;
    let mut final_message = Value::Null;
    let mut text = String::new();
    let mut retry = String::new();
    let mut usage = [0.0_f64; 6];
    for line in stdout.lines().filter(|s| !s.trim().is_empty()) {
        let event: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                invalid += 1;
                continue;
            }
        };
        if !event.is_object() && !event.is_array() {
            continue;
        }
        events += 1;
        match event["type"].as_str().unwrap_or_default() {
            "turn_end" => turns += 1,
            "tool_execution_end" => {
                tools += 1;
                if event["isError"] == true {
                    tool_errors += 1;
                }
            }
            "auto_retry_end" if event["success"] == false => {
                retry = event["finalError"]
                    .as_str()
                    .unwrap_or("Automatic retry failed.")
                    .into()
            }
            "message_end" if event["message"]["role"] == "assistant" => {
                final_message = event["message"].clone();
                for (index, key) in ["input", "output", "cacheRead", "cacheWrite", "totalTokens"]
                    .iter()
                    .enumerate()
                {
                    usage[index] += final_message["usage"][key].as_f64().unwrap_or_default();
                }
                usage[5] += final_message["usage"]["cost"]["total"]
                    .as_f64()
                    .unwrap_or_default();
                let content = final_message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !content.trim().is_empty() {
                    text = content.trim().into();
                }
            }
            _ => {}
        }
    }
    let mut metadata = json!({"command": program, "cwd": cwd.map(|p| p.display().to_string()), "turns": turns, "tool_calls": tools, "tool_errors": tool_errors,
        "exit_code": exit_code, "usage": {"input_tokens": usage[0], "output_tokens": usage[1], "cache_read_tokens": usage[2], "cache_write_tokens": usage[3], "total_tokens": usage[4], "cost_usd": (usage[5] * 1e6).round() / 1e6}});
    for (source, target) in [
        ("provider", "provider"),
        ("model", "model"),
        ("stopReason", "stop_reason"),
    ] {
        if let Some(value) = final_message.get(source) {
            metadata[target] = value.clone();
        }
    }
    if invalid > 0 {
        metadata["invalid_event_lines"] = json!(invalid);
    }
    let mut result = AgentTaskOutcome {
        task_id: request["task_id"].as_str().unwrap_or_default().into(),
        metadata,
        ..Default::default()
    };
    let stop = final_message["stopReason"].as_str().unwrap_or_default();
    let (status, class, code, summary, diagnostic) = if exit_code != Some(0) {
        let tail = stderr
            .trim()
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let reason = final_message["errorMessage"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(if !retry.is_empty() { &retry } else { &tail });
        let reason = if reason.is_empty() {
            format!("Pi exited with status {}.", exit_code.unwrap_or(-1))
        } else {
            reason.into()
        };
        (
            AgentTaskOutcomeStatus::Failed,
            "execution_failed",
            "agent_task.pi_failed",
            truncate(&format!("Pi execution failed: {reason}"), 500),
            format!(
                "Pi exited with status {}. {reason}",
                exit_code.unwrap_or(-1)
            ),
        )
    } else if final_message.is_null() {
        (
            AgentTaskOutcomeStatus::ProviderError,
            "provider",
            "agent_task.pi_no_result",
            "Pi exited without producing an assistant message.".into(),
            if events == 0 {
                "Pi produced no JSON events; check that command_args include --mode json."
            } else {
                "Pi produced events but no completed assistant message."
            }
            .into(),
        )
    } else if stop == "error" || stop == "aborted" || !retry.is_empty() {
        let reason = final_message["errorMessage"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if !retry.is_empty() {
                    retry
                } else {
                    format!("Pi stopped with reason {stop}.")
                }
            });
        (
            AgentTaskOutcomeStatus::ProviderError,
            "provider",
            if stop == "aborted" {
                "agent_task.pi_aborted"
            } else {
                "agent_task.pi_model_error"
            },
            truncate(&format!("Pi model request failed: {reason}"), 500),
            reason,
        )
    } else if stop == "length" {
        (
            AgentTaskOutcomeStatus::Failed,
            "execution_failed",
            "agent_task.pi_output_limit",
            "Pi stopped at the model output limit before finishing.".into(),
            "The final assistant message ended with stopReason \"length\".".into(),
        )
    } else {
        let declared = declarations(request);
        let answer = structured_answer(&text).unwrap_or(Value::Null);
        let values = if declared
            .iter()
            .any(|d| answer["outputs"].get(d["name"].as_str().unwrap()).is_some())
        {
            &answer["outputs"]
        } else {
            &answer
        };
        let mut outputs = serde_json::Map::new();
        let mut oversized = Vec::new();
        for d in &declared {
            let name = d["name"].as_str().unwrap();
            if let Some(value) = values.get(name) {
                if serde_json::to_vec(value).unwrap().len() <= 64 * 1024 {
                    outputs.insert(name.into(), value.clone());
                } else {
                    oversized.push(name);
                }
            }
        }
        let missing = declared
            .iter()
            .filter(|d| d["required"] == true && !outputs.contains_key(d["name"].as_str().unwrap()))
            .map(|d| d["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        if !outputs.is_empty() {
            result.outputs = Value::Object(outputs);
        }
        if !missing.is_empty() {
            result.diagnostics.push(AgentTaskDiagnostic {
                class: "pi.required_outputs_missing".into(),
                message: format!(
                    "Pi completed without required structured output(s): {}.",
                    missing.join(", ")
                ),
                data: json!({"class": "pi.required_outputs_missing", "missing_outputs": missing}),
            });
        }
        if !oversized.is_empty() {
            result.diagnostics.push(AgentTaskDiagnostic { class: "pi.declared_outputs_oversized".into(), message: format!("Pi emitted structured output(s) exceeding the size limit: {}.", oversized.join(", ")), data: json!({"class": "pi.declared_outputs_oversized", "oversized_outputs": oversized}) });
        }
        let summary = if !missing.is_empty() {
            format!(
                "Pi completed without required structured output(s): {}.",
                missing.join(", ")
            )
        } else {
            let paragraph = text.split("\n\n").next().unwrap_or_default().trim();
            if paragraph.is_empty() {
                "Pi completed the task.".into()
            } else if paragraph.chars().count() > 400 {
                format!("{}…", truncate(paragraph, 399))
            } else {
                paragraph.into()
            }
        };
        (
            if missing.is_empty() {
                AgentTaskOutcomeStatus::Succeeded
            } else {
                AgentTaskOutcomeStatus::Failed
            },
            "provider",
            if missing.is_empty() {
                ""
            } else {
                "agent_task.pi_required_outputs_missing"
            },
            summary,
            format!("Pi finished after {turns} turn(s) and {tools} tool call(s)."),
        )
    };
    result.status = status;
    result.summary = Some(summary);
    if !code.is_empty() {
        result.failure_classification = Some(if class == "provider" {
            AgentTaskFailureClassification::Provider
        } else {
            AgentTaskFailureClassification::ExecutionFailed
        });
    }
    result.diagnostics.insert(
        0,
        AgentTaskDiagnostic {
            class: "provider".into(),
            message: diagnostic,
            data: json!({}),
        },
    );
    result
}

pub(super) fn readiness(
    config: &Value,
    env: &[(String, String)],
    timeout: std::time::Duration,
    mode: Option<&str>,
) -> Result<ProviderReadinessInvocationResult, String> {
    let (program, _) = command(config)?;
    let program =
        super::runner_readiness::resolve_executable_candidate(&program).unwrap_or(program);
    let mut env = env.to_vec();
    if let Some(runtime_env) = config["runtime_env"].as_object() {
        env.extend(
            runtime_env
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into()))),
        );
    }
    let timeout = config["readiness_timeout_ms"]
        .as_u64()
        .filter(|ms| *ms > 0)
        .map(|ms| timeout.min(std::time::Duration::from_millis(ms)))
        .unwrap_or(timeout);
    let dir = agent_dir(config, &env);
    let model = config["model"].as_str().unwrap_or_default().trim();
    let started = std::time::Instant::now();
    let mut version = String::new();
    let mut listing = String::new();
    let mut probe_error = None;
    if mode != Some("capacity") {
        for (flag, output) in [("--version", &mut version), ("--list-models", &mut listing)] {
            match super::command_runner::run_pi_readiness_probe(
                &program,
                flag,
                &dir,
                &env,
                timeout.saturating_sub(started.elapsed()),
            ) {
                Ok(text) => *output = text,
                Err(error) => {
                    probe_error = Some(error);
                    break;
                }
            }
        }
    }
    let mut identity = json!({"runtime_id": "pi", "provider_id": "pi.agent-task-executor", "executable": program, "version": version.trim(), "model": model, "agent_dir": dir});
    if let Some(mode) = mode {
        identity["mode"] = json!(mode);
    }
    let cache_key = format!(
        "runtime-readiness:{}",
        homeboy_engine_primitives::content_hash::sha256_hex(
            &serde_json::to_vec(&identity).unwrap()
        )
    );
    identity["cache_key"] = json!(cache_key);
    let routes = listing
        .lines()
        .map(str::trim)
        .filter(|s| !s.to_ascii_lowercase().starts_with("provider "))
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            Some(format!("{}/{}", columns.next()?, columns.next()?))
        })
        .collect::<Vec<_>>();
    let (classification, reason, retryable, remediation) = if mode == Some("capacity") {
        (
            "ready",
            "capacity_not_published",
            false,
            "The selected provider publishes no plan usage endpoint; run the full readiness probe."
                .into(),
        )
    } else if let Some(error) = probe_error {
        ("transient_failure", "model_list_probe_interrupted", true, format!("Retry the bounded Pi model catalog probe; the local runtime process was temporarily unavailable. {error}"))
    } else if listing.to_ascii_lowercase().contains("no models available") || routes.is_empty() {
        ("auth_failure", "no_models_available", false, format!("Log in to a provider for Pi agent directory {dir}, or point PI_CODING_AGENT_DIR at an agent dir that has one."))
    } else if !model.is_empty() && !model.contains('/') {
        let candidates = routes
            .iter()
            .filter(|route| route.rsplit('/').next().unwrap_or_default().contains(model))
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        identity["candidates"] = json!(candidates);
        ("deterministic_incompatibility", "model_not_qualified", false, format!("Select a provider-qualified model available in Pi agent directory {dir}, for example {}.", candidates.join(", ")))
    } else if !model.is_empty() && !routes.iter().any(|route| route == model) {
        ("deterministic_incompatibility", "model_not_available", false, format!("Select a model available in Pi agent directory {dir}, or point PI_CODING_AGENT_DIR at an agent dir that provides {model}."))
    } else {
        ("ready", "", false, String::new())
    };
    Ok(ProviderReadinessInvocationResult {
        ready: classification == "ready",
        classification: classification.into(),
        retryable,
        remediation,
        reason: reason.into(),
        cache_key,
        identity,
        capacity: None,
        durable_identity: None,
    })
}
