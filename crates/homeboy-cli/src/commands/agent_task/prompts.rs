use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::agent_task_prompts;

use super::super::CmdResult;
use super::command_json_value;

#[derive(Args, Debug)]
pub struct AgentTaskPromptsArgs {
    #[command(subcommand)]
    pub command: AgentTaskPromptsCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskPromptsCommand {
    /// Save a markdown prompt in Homeboy's agent-task prompt store.
    Save(AgentTaskPromptSaveArgs),
    /// List stored agent-task prompts.
    List,
    /// Show a stored agent-task prompt.
    Show(AgentTaskPromptNameArgs),
    /// Remove a stored agent-task prompt.
    Remove(AgentTaskPromptNameArgs),
}

#[derive(Args, Debug)]
pub struct AgentTaskPromptSaveArgs {
    /// Stable prompt name. Unsafe path characters are normalized for storage.
    pub name: String,

    /// Prompt markdown content, @file, or - for stdin.
    #[arg(long, value_name = "PROMPT")]
    pub input: String,
}

#[derive(Args, Debug)]
pub struct AgentTaskPromptNameArgs {
    /// Stored prompt name or id.
    pub name: String,
}

#[derive(Debug, Serialize)]
struct PromptSaveReport {
    schema: &'static str,
    id: String,
    path: String,
    reference: String,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
struct PromptListReport {
    schema: &'static str,
    prompt_dir: String,
    prompts: Vec<homeboy::core::agent_task_prompts::AgentTaskPromptRecord>,
}

#[derive(Debug, Serialize)]
struct PromptShowReport {
    schema: &'static str,
    id: String,
    path: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct PromptRemoveReport {
    schema: &'static str,
    removed: bool,
    id: String,
    path: String,
}

pub fn prompts(args: AgentTaskPromptsArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskPromptsCommand::Save(args) => save(args),
        AgentTaskPromptsCommand::List => list(),
        AgentTaskPromptsCommand::Show(args) => show(args),
        AgentTaskPromptsCommand::Remove(args) => remove(args),
    }
}

fn save(args: AgentTaskPromptSaveArgs) -> CmdResult<Value> {
    let content = agent_task_prompts::read_prompt_input(&args.input)?;
    let record = agent_task_prompts::save_prompt(&args.name, &content)?;
    Ok((
        command_json_value(PromptSaveReport {
            schema: "homeboy/agent-task-prompt/v1",
            id: record.id.clone(),
            path: record.path,
            reference: format!("{}{}", agent_task_prompts::PROMPT_REF_PREFIX, record.id),
            size_bytes: record.size_bytes,
        })?,
        0,
    ))
}

fn list() -> CmdResult<Value> {
    Ok((
        command_json_value(PromptListReport {
            schema: "homeboy/agent-task-prompts/v1",
            prompt_dir: agent_task_prompts::prompts_dir()?.display().to_string(),
            prompts: agent_task_prompts::list_prompts()?,
        })?,
        0,
    ))
}

fn show(args: AgentTaskPromptNameArgs) -> CmdResult<Value> {
    let record = agent_task_prompts::prompt_path(&args.name)?;
    Ok((
        command_json_value(PromptShowReport {
            schema: "homeboy/agent-task-prompt-content/v1",
            id: agent_task_prompts::prompt_id(&args.name)?,
            path: record.display().to_string(),
            content: agent_task_prompts::read_prompt(&args.name)?,
        })?,
        0,
    ))
}

fn remove(args: AgentTaskPromptNameArgs) -> CmdResult<Value> {
    let record = agent_task_prompts::remove_prompt(&args.name)?;
    Ok((
        command_json_value(PromptRemoveReport {
            schema: "homeboy/agent-task-prompt-remove/v1",
            removed: true,
            id: record.id,
            path: record.path,
        })?,
        0,
    ))
}
