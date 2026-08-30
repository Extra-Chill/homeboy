use super::*;

pub(crate) fn prepare_capability_run(
    execution_context: &ExtensionExecutionContext,
    pre_loaded_component: Option<&Component>,
    path_override: Option<&str>,
    settings_overrides: &[(String, String)],
    settings_json_overrides: &[(String, serde_json::Value)],
    skip_script_validation: bool,
) -> Result<PreparedCapabilityRun> {
    let component =
        resolve_capability_component(execution_context, pre_loaded_component, path_override)?;
    let execution = build_capability_execution_context(execution_context, component, path_override);

    // Skip validation when a command_override is provided (e.g., Build with command_template)
    // since the script_path may be empty or not point to an actual file.
    if !skip_script_validation && !execution.script_path.is_empty() {
        validate_capability_script_exists(
            &execution.extension_path,
            &execution.script_path,
            execution.capability,
        )?;
    }

    let manifest = load_extension_manifest_from_dir(&execution.extension_path)?;
    homeboy_extension_contract::validate_core_compatibility(
        "extension",
        &execution.extension_id,
        manifest
            .get("requires")
            .and_then(|requires| requires.get("homeboy"))
            .and_then(serde_json::Value::as_str),
        homeboy_core::extension_update_check::read_source_revision(&execution.extension_id),
    )?;
    let settings_json = build_settings_json_from_manifest(
        &manifest,
        &execution.settings,
        settings_overrides,
        settings_json_overrides,
    )?;

    Ok(PreparedCapabilityRun {
        execution,
        settings_json,
    })
}

fn build_template_vars<'a>(
    extension_path: &'a str,
    args_str: &'a str,
    runtime: &'a RuntimeConfig,
    project: Option<&'a Project>,
    project_id: &'a Option<String>,
) -> Vec<(&'a str, &'a str)> {
    let entrypoint = runtime.entrypoint.as_deref().unwrap_or("");

    if let Some(proj) = project {
        let domain = proj.domain.as_deref().unwrap_or("");
        let site_path = proj.base_path.as_deref().unwrap_or("");
        vec![
            ("extension_path", extension_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
            ("projectId", project_id.as_deref().unwrap_or("")),
            ("domain", domain),
            ("sitePath", site_path),
        ]
    } else {
        vec![
            ("extension_path", extension_path),
            ("entrypoint", entrypoint),
            ("args", args_str),
        ]
    }
}

fn build_runtime_env(
    runtime: &RuntimeConfig,
    context: &ResolvedExtensionInvocationContext,
    vars: &[(&str, &str)],
    settings_json: &str,
    extension_path: &str,
) -> Vec<(String, String)> {
    let project_base_path = context
        .project
        .as_ref()
        .and_then(|p| p.base_path.as_deref());

    let mut env = build_exec_env(
        &context.extension_id,
        context.project_id.as_deref(),
        context
            .component
            .as_ref()
            .map(|component| component.id.as_str()),
        settings_json,
        Some(extension_path),
        project_base_path,
        Some(&context.settings),
        context
            .component
            .as_ref()
            .map(|component| component.local_path.as_str()),
    );

    if let Some(ref extension_env) = runtime.env {
        for (key, value) in extension_env {
            let rendered_value = template::render(value, vars);
            env.push((key.clone(), rendered_value));
        }
    }

    env
}

pub(super) fn build_action_env(
    extension_id: &str,
    project_id: Option<&str>,
    payload: &serde_json::Value,
    extension_path: Option<&str>,
    project_base_path: Option<&str>,
) -> Vec<(String, String)> {
    let settings_json = payload.to_string();
    let component_id =
        homeboy_engine_primitives::text::json_path_str(payload, &["release", "component_id"]);
    let component_path =
        homeboy_engine_primitives::text::json_path_str(payload, &["release", "local_path"]);

    let mut env = build_exec_env(
        extension_id,
        project_id,
        component_id,
        &settings_json,
        extension_path,
        project_base_path,
        None,
        component_path,
    );
    if let Some(source_path) =
        homeboy_engine_primitives::text::json_path_str(payload, &["release", "source_path"])
    {
        env.push((
            "HOMEBOY_RELEASE_SOURCE_PATH".to_string(),
            source_path.to_string(),
        ));
    }
    env
}

pub(super) fn execute_extension_command(
    command_template: &str,
    vars: &[(&str, &str)],
    working_dir: Option<&str>,
    env_pairs: &[(String, String)],
    mode: ExtensionExecutionMode,
) -> Result<ExtensionExecutionResult> {
    let command = template::render(command_template, vars);
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    match mode {
        ExtensionExecutionMode::Interactive => {
            let exit_code =
                execute_local_command_interactive(&command, working_dir, Some(&env_refs));
            Ok(ExtensionExecutionResult {
                output: CapturedOutput::default(),
                exit_code,
                success: exit_code == 0,
            })
        }
        ExtensionExecutionMode::Captured => {
            let cmd_output = execute_local_command_in_dir(&command, working_dir, Some(&env_refs));
            Ok(ExtensionExecutionResult {
                output: CapturedOutput::new(cmd_output.stdout, cmd_output.stderr),
                exit_code: cmd_output.exit_code,
                success: cmd_output.success,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_extension_runtime(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    inputs: Vec<(String, String)>,
    args: Vec<String>,
    payload: Option<&serde_json::Value>,
    working_dir: Option<&str>,
    mode: ExtensionExecutionMode,
    filter: &ExtensionStepFilter,
) -> Result<ExtensionExecutionOutcome> {
    // Shell execution is required for extension runtime commands by design:
    // - Runtime commands execute bash scripts (set -euo pipefail, arrays, jq)
    // - Scripts use bash features (arrays, variable expansion, subshells)
    // - Commands like "{{extensionPath}}/scripts/publish-github.sh" need shell
    // - Environment variable passing requires shell environment
    // - Direct execution cannot handle bash scripts or shell features
    // See executor.rs for detailed execution strategy decision tree
    let extension = load_extension(extension_id)?;
    homeboy_extension_contract::validate_core_compatibility(
        "extension",
        extension_id,
        extension
            .requires
            .as_ref()
            .and_then(|requires| requires.homeboy.as_deref()),
        homeboy_core::extension_update_check::read_source_revision(extension_id),
    )?;
    let runtime = extension_runtime(&extension)?;
    let run_command = runtime.run_command.as_ref().ok_or_else(|| {
        Error::config(format!(
            "Extension '{}' does not have a runCommand defined",
            extension_id
        ))
    })?;

    let extension_path = validation::require(
        extension.extension_path.as_ref(),
        "extension",
        "extension_path not set",
    )?;

    let args_str = build_args_string(&extension, inputs, args);
    let context = ResolvedExtensionInvocationContext::resolve_runtime(
        &extension,
        extension_id,
        project_id,
        component_id,
        run_command,
    )?;

    let settings_json = if let Some(payload) = payload {
        payload.to_string()
    } else {
        serialize_settings(&context.settings)?
    };

    let vars = build_template_vars(
        extension_path,
        &args_str,
        runtime,
        context.project.as_ref(),
        &context.project_id,
    );
    let mut env_pairs = build_runtime_env(runtime, &context, &vars, &settings_json, extension_path);

    env_pairs.extend(filter.to_env_pairs());

    let execution = execute_extension_command(
        run_command,
        &vars,
        working_dir.or(Some(extension_path.as_str())),
        &env_pairs,
        mode,
    )?;

    Ok(ExtensionExecutionOutcome {
        project_id: context.project_id,
        result: execution,
    })
}

/// Build execution environment variables for a extension.
///
/// This is the single canonical env builder for all extension execution contexts
/// (test, lint, build, extension run, deploy hooks, action handlers).
///
/// When `component_path_override` is provided, it is used as the component path
/// instead of loading the component from storage. This supports `--path` overrides
/// in commands like `homeboy test --path /alt/path`.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_exec_env(
    extension_id: &str,
    project_id: Option<&str>,
    component_id: Option<&str>,
    settings_json: &str,
    extension_path: Option<&str>,
    project_base_path: Option<&str>,
    settings: Option<&HashMap<String, serde_json::Value>>,
    component_path_override: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = vec![
        (
            exec_context::VERSION.to_string(),
            exec_context::CURRENT_VERSION.to_string(),
        ),
        (
            exec_context::EXTENSION_ID.to_string(),
            extension_id.to_string(),
        ),
        (
            exec_context::SETTINGS_JSON.to_string(),
            settings_json.to_string(),
        ),
    ];

    if let Some(pid) = project_id {
        env.push((exec_context::PROJECT_ID.to_string(), pid.to_string()));
    }

    if let Some(cid) = component_id {
        env.push((exec_context::COMPONENT_ID.to_string(), cid.to_string()));

        // Use override path if provided, otherwise load from storage
        let component_path = if let Some(override_path) = component_path_override {
            override_path.to_string()
        } else {
            match component::resolve_effective(Some(cid), None, None) {
                Ok(component) => component.local_path,
                Err(e) => {
                    env.push(("HOMEBOY_COMPONENT_LOAD_ERROR".to_string(), e.to_string()));
                    format!("/debug/component-not-found/{}", cid)
                }
            }
        };
        env.push((exec_context::COMPONENT_PATH.to_string(), component_path));
    }

    if let Some(mp) = extension_path {
        env.push((exec_context::EXTENSION_PATH.to_string(), mp.to_string()));
    }

    if let Ok(helper_pairs) = runtime_helper::ensure_all_helpers() {
        env.extend(helper_pairs);
    }

    // No rig context reaches this exec-env builder yet, so this resolves to the
    // built-in default. The provider now takes a rig id so a rig-aware caller
    // can thread its declaration through without another signature change.
    if let Some(path) = homeboy_core::rig_toolchain_provider::command_step_path(None) {
        env.push(("PATH".to_string(), path.to_string_lossy().to_string()));
    }

    if let Some(pbp) = project_base_path {
        env.push((exec_context::PROJECT_PATH.to_string(), pbp.to_string()));
    }

    if let Some(settings_map) = settings {
        for (key, value) in settings_map {
            let env_key = format!("HOMEBOY_SETTINGS_{}", key.to_uppercase());
            let env_value = match value {
                serde_json::Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            env.push((env_key, env_value));
        }
    }

    env
}
