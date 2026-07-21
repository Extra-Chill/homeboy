//! Prompt- and task-spec parsing for agent-task dispatch.
//!
//! Reads cook prompt/tasks-json inputs (raw strings, stored-prompt refs, and
//! `--tasks-json` arrays) into validated [`DispatchPromptSpec`]s, resolving
//! stored prompts and deriving deterministic task ids. Extracted from
//! `agent_task_dispatch_plan` to keep the plan builder focused on plan shape.

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agent_task_prompts;
use homeboy_core::{config, Error, Result};

pub(crate) fn read_text_spec(spec: &str, label: &str) -> Result<String> {
    if let Some(prompt) = agent_task_prompts::resolve_stored_prompt_ref(spec)? {
        return Ok(prompt);
    }

    config::read_json_spec_to_string(spec).map_err(|error| {
        Error::internal_unexpected(format!(
            "failed to read agent-task cook {label} input: {error}"
        ))
    })
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct StoredPromptSource {
    pub(crate) id: String,
    pub(crate) reference: String,
    pub(crate) sha256: String,
}

pub(crate) struct ResolvedPromptSpec {
    pub(crate) content: String,
    pub(crate) stored_prompt: Option<StoredPromptSource>,
}

pub(crate) fn read_prompt_spec(spec: &str) -> Result<ResolvedPromptSpec> {
    if let Some(id) = agent_task_prompts::stored_prompt_ref_id(spec) {
        let content = agent_task_prompts::read_prompt(id)?;
        let sha256 = format!("sha256:{:x}", Sha256::digest(content.as_bytes()));
        let id = agent_task_prompts::prompt_id(id)?;
        return Ok(ResolvedPromptSpec {
            content,
            stored_prompt: Some(StoredPromptSource {
                reference: format!("{}{}", agent_task_prompts::PROMPT_REF_PREFIX, id),
                id,
                sha256,
            }),
        });
    }

    Ok(ResolvedPromptSpec {
        content: read_text_spec(spec, "prompt")?,
        stored_prompt: None,
    })
}

pub(crate) struct DispatchPromptSpec {
    pub(crate) prompt: String,
    pub(crate) task_id: Option<String>,
}

impl DispatchPromptSpec {
    pub(crate) fn new(prompt: String) -> Self {
        Self {
            prompt,
            task_id: None,
        }
    }
}

pub(crate) fn read_dispatch_tasks_json(spec: Option<&str>) -> Result<Vec<DispatchPromptSpec>> {
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };

    let raw = read_text_spec(spec, "tasks")?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task cook tasks".to_string()),
            Some(raw.clone()),
        )
    })?;

    match value {
        Value::Array(items) => task_prompts_from_json_items(items),
        Value::Object(mut object) => match object.remove("tasks") {
            Some(Value::Array(items)) => task_prompts_from_json_items(items),
            _ => Err(invalid_tasks_json_error()),
        },
        _ => Err(invalid_tasks_json_error()),
    }
}

fn task_prompts_from_json_items(items: Vec<Value>) -> Result<Vec<DispatchPromptSpec>> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| task_prompt_from_json_item(item, index))
        .collect()
}

fn invalid_tasks_json_error() -> Error {
    Error::validation_invalid_argument(
        "tasks",
        "agent-task cook --tasks expects a JSON array or object with a tasks array",
        None,
        None,
    )
}

fn task_prompt_from_json_item(item: Value, index: usize) -> Result<DispatchPromptSpec> {
    match item {
        Value::String(prompt) => {
            validate_task_prompt(&prompt, index)?;
            Ok(DispatchPromptSpec::new(prompt))
        }
        Value::Object(mut object) => {
            validate_task_object_keys(&object, index)?;
            let prompt = object
                .remove("prompt")
                .or_else(|| object.remove("instructions"))
                .and_then(|value| value.as_str().map(str::to_string))
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "tasks",
                        format!(
                            "agent-task cook task item {} must include a string prompt or instructions field",
                            index + 1
                        ),
                        None,
                        None,
                    )
                })?;
            validate_task_prompt(&prompt, index)?;
            let task_id = object
                .remove("task_id")
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "tasks",
                            format!(
                                "agent-task cook task item {} task_id field must be a string",
                                index + 1
                            ),
                            None,
                            None,
                        )
                    })
                })
                .transpose()?;
            Ok(DispatchPromptSpec { prompt, task_id })
        }
        _ => Err(Error::validation_invalid_argument(
            "tasks",
            format!(
                "agent-task cook task item {} must be a string or object",
                index + 1
            ),
            None,
            None,
        )),
    }
}

fn validate_task_prompt(prompt: &str, index: usize) -> Result<()> {
    if !prompt.trim().is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "tasks",
        format!(
            "agent-task cook task item {} must include a non-empty prompt or instructions field",
            index + 1
        ),
        None,
        None,
    ))
}

fn validate_task_object_keys(object: &serde_json::Map<String, Value>, index: usize) -> Result<()> {
    for key in object.keys() {
        if key != "prompt" && key != "instructions" && key != "task_id" {
            return Err(Error::validation_invalid_argument(
                "tasks",
                format!(
                    "agent-task cook task item {} includes unsupported field {key:?}; supported fields are prompt, instructions, and task_id",
                    index + 1
                ),
                None,
                None,
            ));
        }
    }
    Ok(())
}

pub(crate) fn dispatch_task_id(repo: Option<&str>, index: usize) -> String {
    let slug = repo
        .map(sanitize_slug)
        .unwrap_or_else(|| "task".to_string());
    if index == 0 {
        format!("cook-{slug}")
    } else {
        format!("cook-{slug}-{}", index + 1)
    }
}

pub(crate) fn explicit_dispatch_task_id(task_id: &str, index: usize) -> String {
    let base = sanitize_slug(task_id);
    if index == 0 {
        base
    } else {
        format!("{base}-{}", index + 1)
    }
}

fn sanitize_slug(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}
