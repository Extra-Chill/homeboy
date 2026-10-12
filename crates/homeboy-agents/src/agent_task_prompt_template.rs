//! Operator-owned prompt templates (#15818).
//!
//! Homeboy is orchestration infrastructure: it authors no agent-facing prose.
//! The model-visible instructions are the operator's prompt plus, when the
//! operator declared a template for the purpose in
//! `agent_task.prompt_templates`, that template rendered against structured
//! Cook facts. Facts (gates, publication ownership, command policy, declared
//! outputs, gate failures) travel as data on the request, never as English
//! sentences baked into this binary.

use serde_json::{json, Value};

use crate::agent_task::AgentTaskRequest;
use homeboy_core::defaults;

/// Request-metadata key holding the operator's prompt before any template
/// was appended. Follow-up attempts always render from this base so templates
/// never accumulate across attempts.
pub(crate) const BASE_INSTRUCTIONS_METADATA_KEY: &str = "homeboy_prompt_base_instructions";

/// Request-input key under which Homeboy places structured Cook facts.
pub(crate) const HOMEBOY_CONTEXT_INPUT_KEY: &str = "homeboy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptPurpose {
    /// The first provider attempt of a Cook.
    Attempt,
    /// A gate-fix attempt after deterministic gates failed.
    FollowUp,
    /// An attempt whose remaining work is the review form.
    ReviewForm,
}

impl PromptPurpose {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Attempt => "attempt",
            Self::FollowUp => "follow_up",
            Self::ReviewForm => "review_form",
        }
    }
}

/// Merge `facts` into `request.inputs.homeboy`, preserving existing keys.
pub(crate) fn merge_homeboy_facts(request: &mut AgentTaskRequest, facts: Value) {
    if !request.inputs.is_object() {
        request.inputs = json!({});
    }
    let slot = &mut request.inputs[HOMEBOY_CONTEXT_INPUT_KEY];
    if !slot.is_object() {
        *slot = json!({});
    }
    if let (Some(slot), Value::Object(facts)) = (slot.as_object_mut(), facts) {
        for (key, value) in facts {
            slot.insert(key, value);
        }
    }
}

/// Set the request's instructions to the operator base prompt plus the
/// operator-declared template for `purpose`, using the host config.
///
/// A template value may be inline text or a stored prompt reference
/// (`prompt:<name>`, managed with `homeboy agent-task prompts`).
pub(crate) fn apply_prompt_template(request: &mut AgentTaskRequest, purpose: PromptPurpose) {
    let template = configured_template(purpose);
    apply_prompt_template_with(request, purpose, template.as_deref());
}

fn configured_template(purpose: PromptPurpose) -> Option<String> {
    let value = defaults::load_config()
        .agent_task
        .prompt_templates
        .remove(purpose.as_str())?;
    match crate::agent_task_prompts::resolve_stored_prompt_ref(&value) {
        Ok(Some(content)) => Some(content),
        Ok(None) => Some(value),
        Err(error) => {
            eprintln!(
                "homeboy: agent_task.prompt_templates.{} could not be resolved: {}",
                purpose.as_str(),
                error.message
            );
            None
        }
    }
}

pub(crate) fn apply_prompt_template_with(
    request: &mut AgentTaskRequest,
    purpose: PromptPurpose,
    template: Option<&str>,
) {
    if !request.metadata.is_object() {
        request.metadata = json!({});
    }
    let base = request.metadata[BASE_INSTRUCTIONS_METADATA_KEY]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| request.instructions.clone());
    request.metadata[BASE_INSTRUCTIONS_METADATA_KEY] = Value::String(base.clone());

    let rendered = template
        .map(|template| render(template, &template_context(request, &base, purpose)))
        .map(|rendered| rendered.trim().to_string())
        .filter(|rendered| !rendered.is_empty());
    request.instructions = match rendered {
        Some(rendered) => format!("{base}\n\n{rendered}"),
        None => base,
    };
}

fn template_context(request: &AgentTaskRequest, base: &str, purpose: PromptPurpose) -> Value {
    json!({
        "purpose": purpose.as_str(),
        "instructions": base,
        "task_id": request.task_id,
        "inputs": request.inputs,
        "policy": serde_json::to_value(&request.policy).unwrap_or(Value::Null),
        "outputs": serde_json::to_value(&request.output_declarations).unwrap_or(Value::Null),
        "metadata": request.metadata,
    })
}

/// Render `{{ dotted.path }}` placeholders against `context`. Strings render
/// raw, other values as pretty JSON, and missing paths as empty strings.
pub(crate) fn render(template: &str, context: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let path = after[..end].trim();
        out.push_str(&lookup(context, path));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn lookup(context: &Value, path: &str) -> String {
    let pointer = format!("/{}", path.replace('.', "/"));
    match context.pointer(&pointer) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(value) => serde_json::to_string_pretty(value).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };

    fn request(instructions: &str) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "cook-example".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: instructions.to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: Value::Null,
        }
    }

    #[test]
    fn without_a_template_the_model_sees_only_the_operator_prompt() {
        let mut request = request("Fix the bug.");
        merge_homeboy_facts(
            &mut request,
            json!({ "gates": { "verify": ["cargo test"] } }),
        );
        apply_prompt_template_with(&mut request, PromptPurpose::Attempt, None);
        assert_eq!(request.instructions, "Fix the bug.");
        assert_eq!(
            request.inputs["homeboy"]["gates"]["verify"][0],
            "cargo test"
        );
    }

    #[test]
    fn templates_render_structured_facts_and_never_accumulate() {
        let mut request = request("Fix the bug.");
        merge_homeboy_facts(
            &mut request,
            json!({ "gates": { "verify": ["cargo test"] } }),
        );
        apply_prompt_template_with(
            &mut request,
            PromptPurpose::Attempt,
            Some("Gates: {{ inputs.homeboy.gates.verify }} ({{purpose}}){{ missing.path }}"),
        );
        assert_eq!(
            request.instructions,
            "Fix the bug.\n\nGates: [\n  \"cargo test\"\n] (attempt)"
        );

        apply_prompt_template_with(
            &mut request,
            PromptPurpose::FollowUp,
            Some("Next: {{purpose}}"),
        );
        assert_eq!(request.instructions, "Fix the bug.\n\nNext: follow_up");
        apply_prompt_template_with(&mut request, PromptPurpose::FollowUp, None);
        assert_eq!(request.instructions, "Fix the bug.");
    }

    #[test]
    fn unterminated_placeholders_render_literally() {
        assert_eq!(render("a {{ b", &json!({})), "a {{ b");
    }

    /// Regression audit (#15818): the prompt-assembly paths must not regrow
    /// Homeboy-authored agent instructions. Facts go on the request as data;
    /// wording belongs to `agent_task.prompt_templates`.
    #[test]
    fn prompt_assembly_paths_contain_no_homeboy_authored_agent_prose() {
        let sources = [
            (
                "agent_task_service/cook.rs",
                include_str!("agent_task_service/cook.rs"),
            ),
            (
                "agent_task_cook_loop.rs",
                include_str!("agent_task_cook_loop.rs"),
            ),
            (
                "agent_task_dispatch_plan/mod.rs",
                include_str!("agent_task_dispatch_plan/mod.rs"),
            ),
            (
                "agent_task_review_dossier.rs",
                include_str!("agent_task_review_dossier.rs"),
            ),
            (
                "agent_task/command_policy.rs",
                include_str!("agent_task/command_policy.rs"),
            ),
        ];
        let banned = [
            concat!("Declared deterministic gates ", "are controller-owned"),
            concat!("non-publishable ", "attempt workspace"),
            concat!("reviewer-facing ", "PR dossier"),
            concat!("Continue the Homeboy ", "cook loop"),
            concat!("Generated change ", "guardrails"),
            concat!("Parallel worktree ", "patch preservation"),
            concat!("Command policy (declared ", "by the operator"),
            concat!("Do not run or report ", "verification commands"),
            concat!("Do not claim a focused ", "check"),
        ];
        for (path, source) in sources {
            for phrase in banned {
                assert!(
                    !source.contains(phrase),
                    "{path} reintroduces Homeboy-authored agent prose: {phrase:?}"
                );
            }
        }
    }
}
