use std::io::Read;

use serde_json::{json, Value};

use homeboy::core::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::scheduler::AgentTaskScheduleOptions;
use homeboy::core::agent_tasks::{
    build_wordpress_runtime_plan, dla_extraction_task, AgentTaskLimits,
    WordPressRuntimeExecutorSpec, WordPressRuntimePlanRequest, WordPressRuntimeTaskSpec,
    WORDPRESS_RUNTIME_PLAN_REQUEST_SCHEMA,
};

use super::args::WordPressRuntimeArgs;
use super::run::run_loaded_plan;
use crate::commands::CmdResult;

pub(super) fn wordpress_runtime(args: WordPressRuntimeArgs) -> CmdResult<Value> {
    if args.submit && args.run {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "submit",
            "use either --submit or --run, not both",
            None,
            None,
        ));
    }

    let plan_request = plan_request_from_args(&args)?;
    let plan = build_wordpress_runtime_plan(plan_request);

    if args.submit {
        let record = agent_task_lifecycle::submit_plan(&plan, args.run_id.as_deref())?;
        return Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0));
    }

    if args.run {
        let record_run_id = args.record.then_some(args.run_id.as_deref()).flatten();
        return run_loaded_plan(
            plan,
            record_run_id,
            ExtensionProviderAgentTaskExecutor::discover(),
        );
    }

    Ok((serde_json::to_value(plan).unwrap_or(Value::Null), 0))
}

fn plan_request_from_args(
    args: &WordPressRuntimeArgs,
) -> homeboy::core::Result<WordPressRuntimePlanRequest> {
    if let Some(spec) = &args.spec {
        let mut request: WordPressRuntimePlanRequest =
            serde_json::from_value(read_json_value(spec)?).map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "spec",
                    format!("invalid WordPressRuntimePlanRequest JSON: {error}"),
                    None,
                    None,
                )
            })?;
        apply_cli_overrides(args, &mut request);
        return Ok(request);
    }

    let mut tasks: Vec<WordPressRuntimeTaskSpec> = Vec::new();
    for runtime_task in &args.runtime_task {
        tasks.push(task_from_runtime_value(
            read_json_value(runtime_task)?,
            args,
        ));
    }
    if let Some(ability) = &args.ability {
        let input = args
            .ability_input
            .as_ref()
            .map(|value| read_json_value(value))
            .transpose()?
            .unwrap_or_else(|| json!({}));
        tasks.push(task_from_runtime_value(
            json!({
                "ability": ability,
                "input": input,
            }),
            args,
        ));
    }
    for url in &args.dla_url {
        let mut task = dla_extraction_task(url.clone());
        merge_executor_overrides(args, &mut task.executor);
        apply_task_overrides(args, &mut task);
        tasks.push(task);
    }

    if tasks.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runtime-task",
            "provide --spec, --runtime-task, --ability, or --dla-url",
            None,
            None,
        ));
    }

    Ok(WordPressRuntimePlanRequest {
        schema: WORDPRESS_RUNTIME_PLAN_REQUEST_SCHEMA.to_string(),
        plan_id: args.plan_id.clone(),
        group_key: args.group_key.clone(),
        tasks,
        options: AgentTaskScheduleOptions {
            max_concurrency: args.max_concurrency.max(1),
            ..AgentTaskScheduleOptions::default()
        },
        component_contracts: Vec::new(),
        metadata: Value::Null,
    })
}

fn task_from_runtime_value(
    runtime_task: Value,
    args: &WordPressRuntimeArgs,
) -> WordPressRuntimeTaskSpec {
    let mut task = WordPressRuntimeTaskSpec {
        task_id: None,
        kind: Some("runtime_task".to_string()),
        instructions: None,
        runtime_task,
        executor: WordPressRuntimeExecutorSpec::default(),
        source_refs: Vec::new(),
        component_contracts: Vec::new(),
        policy: Default::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        metadata: Value::Null,
    };
    merge_executor_overrides(args, &mut task.executor);
    apply_task_overrides(args, &mut task);
    task
}

fn apply_cli_overrides(args: &WordPressRuntimeArgs, request: &mut WordPressRuntimePlanRequest) {
    if args.plan_id.is_some() {
        request.plan_id = args.plan_id.clone();
    }
    if args.group_key.is_some() {
        request.group_key = args.group_key.clone();
    }
    if args.max_concurrency != 1 {
        request.options.max_concurrency = args.max_concurrency;
    }
    for task in &mut request.tasks {
        merge_executor_overrides(args, &mut task.executor);
        apply_task_overrides(args, task);
    }
}

fn apply_task_overrides(args: &WordPressRuntimeArgs, task: &mut WordPressRuntimeTaskSpec) {
    if let Some(timeout_ms) = args.timeout_ms {
        task.limits.timeout_ms = Some(timeout_ms);
    }
    for artifact in &args.expected_artifact {
        if !task
            .expected_artifacts
            .iter()
            .any(|existing| existing == artifact)
        {
            task.expected_artifacts.push(artifact.clone());
        }
    }
}

fn merge_executor_overrides(
    args: &WordPressRuntimeArgs,
    executor: &mut WordPressRuntimeExecutorSpec,
) {
    executor.backend = args.backend.clone();
    executor.selector = args.selector.clone().or_else(|| executor.selector.clone());
    executor.runtime_id = Some(args.runtime_id.clone());
    executor.provider = args.provider.clone().or_else(|| executor.provider.clone());
    executor.model = args.model.clone().or_else(|| executor.model.clone());
    executor.substrate_ref = args
        .substrate_ref
        .clone()
        .or_else(|| executor.substrate_ref.clone());
    for capability in &args.capability {
        if !executor
            .required_capabilities
            .iter()
            .any(|existing| existing == capability)
        {
            executor.required_capabilities.push(capability.clone());
        }
    }
    for secret_env in &args.secret_env {
        if !executor
            .secret_env
            .iter()
            .any(|existing| existing == secret_env)
        {
            executor.secret_env.push(secret_env.clone());
        }
    }
}

fn read_json_value(input: &str) -> homeboy::core::Result<Value> {
    let raw = if input == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "json",
                    format!("failed to read stdin: {error}"),
                    None,
                    None,
                )
            })?;
        buffer
    } else if let Some(path) = input.strip_prefix('@') {
        std::fs::read_to_string(path).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "json",
                format!("failed to read {path}: {error}"),
                None,
                None,
            )
        })?
    } else {
        input.to_string()
    };

    serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "json",
            format!("invalid JSON: {error}"),
            None,
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_build_dla_plan_without_local_execution() {
        let args = WordPressRuntimeArgs {
            spec: None,
            runtime_task: Vec::new(),
            ability: None,
            ability_input: None,
            dla_url: vec!["https://example.com".to_string()],
            plan_id: Some("dla-run".to_string()),
            group_key: None,
            run_id: None,
            backend: "codebox".to_string(),
            selector: Some("lab-codebox".to_string()),
            runtime_id: "wp-codebox".to_string(),
            provider: None,
            model: None,
            substrate_ref: Some("main".to_string()),
            capability: Vec::new(),
            secret_env: Vec::new(),
            expected_artifact: vec!["import-manifest".to_string()],
            max_concurrency: 1,
            timeout_ms: Some(120_000),
            submit: false,
            run: false,
            record: false,
        };

        let request = plan_request_from_args(&args).expect("request");
        let plan = build_wordpress_runtime_plan(request);
        let task = &plan.tasks[0];

        assert_eq!(task.executor.backend, "codebox");
        assert_eq!(task.executor.executor_provider_id(), Some("lab-codebox"));
        assert_eq!(task.executor.substrate_ref(), Some("main"));
        assert_eq!(
            task.inputs["runtime_task"]["source"]["url"],
            "https://example.com"
        );
        assert_eq!(task.limits.timeout_ms, Some(120_000));
        assert!(task
            .artifact_declarations
            .iter()
            .any(|artifact| artifact.name == "import-manifest"));
    }
}
