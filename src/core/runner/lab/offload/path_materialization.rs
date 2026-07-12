//! Controller-side discovery of every Lab path materialization input.
//!
//! The planner is deliberately evaluated before workspace mutation.  Staging
//! consumes its resolved argv and workspace list rather than rediscovering
//! inputs after individual argument transforms.

use super::*;
use crate::core::rig;

pub(crate) struct PathMaterializationPlanner {
    pub(crate) args: Vec<String>,
    pub(crate) extra_workspaces: Vec<ExtraLabWorkspace>,
}

impl PathMaterializationPlanner {
    pub(crate) fn plan(
        args: &[String],
        contract: &LabOffloadCommand,
        source_path: &Path,
        allow_dirty_lab_workspace: bool,
    ) -> Result<Self> {
        let (args, workspace_ref_resolutions) = resolve_path_setting_workspace_refs_in_args(args)?;
        let mut extra_workspaces = lab_extra_workspaces(source_path)?;
        extra_workspaces.extend(provider_config_extra_workspaces(&args, source_path)?);
        extra_workspaces.extend(agent_task_plan_extra_workspaces(&args, source_path)?);
        extra_workspaces.extend(agent_task_fanout_extra_workspaces(&args, source_path)?);
        extra_workspaces.extend(agent_task_provider_runtime_component_extra_workspaces(
            &args,
            source_path,
        )?);
        extra_workspaces.extend(workspace_ref_extra_workspaces(
            &workspace_ref_resolutions,
            source_path,
        )?);
        extra_workspaces.extend(path_setting_extra_workspaces(&args, source_path)?);
        extra_workspaces.extend(rig_declared_path_input_extra_workspaces(
            &args,
            contract.workload.as_ref(),
            source_path,
        )?);
        extra_workspaces.extend(runtime_refresh_source_extra_workspaces(
            &args,
            source_path,
            allow_dirty_lab_workspace,
        )?);
        extra_workspaces.extend(extension_source_extra_workspaces(
            &args,
            source_path,
            allow_dirty_lab_workspace,
        )?);
        extra_workspaces.extend(rig_component_path_env_extra_workspaces(source_path)?);

        Ok(Self {
            args,
            extra_workspaces,
        })
    }
}

pub(crate) fn rig_declared_path_input_extra_workspaces(
    args: &[String],
    workload: Option<&crate::command_contract::LabRigWorkloadArguments>,
    primary_source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    if !workload.is_some_and(|workload| {
        matches!(
            workload.kind,
            crate::command_contract::LabRigWorkloadKind::Bench
        )
    }) {
        return Ok(Vec::new());
    }

    let mut path_inputs = std::collections::BTreeSet::new();
    for rig_id in &workload.expect("checked above").rig_ids {
        let Some(spec) = load_primary_rig_spec(primary_source_path, rig_id)? else {
            continue;
        };
        if let Some(bench) = spec.bench.as_ref() {
            path_inputs.extend(bench.path_inputs.iter().cloned());
        }
    }

    path_values_extra_workspaces(
        declared_path_input_values(args, &path_inputs.into_iter().collect::<Vec<_>>()),
        primary_source_path,
        "rig_path_input",
    )
}

fn load_primary_rig_spec(primary_source_path: &Path, rig_id: &str) -> Result<Option<rig::RigSpec>> {
    if !primary_source_path.join("rig.json").is_file() && !primary_source_path.join("rigs").is_dir()
    {
        return Ok(None);
    }
    let Some(discovered) = rig::discover_rigs(primary_source_path)?
        .into_iter()
        .find(|candidate| candidate.id == rig_id)
    else {
        return Ok(None);
    };
    Ok(Some(rig::load_local_source(
        &discovered.rig_path.to_string_lossy(),
        Some(discovered.id.as_str()),
    )?))
}

pub(crate) fn declared_path_input_values(args: &[String], path_inputs: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    for input in path_inputs {
        if input.starts_with("--") {
            values.extend(values_for_flag_including_passthrough(args, input));
        } else {
            values.extend(values_for_setting_path(args, input));
        }
    }
    values
}

fn values_for_flag_including_passthrough(args: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == flag {
            if let Some(value) = iter.peek() {
                values.push((*value).to_string());
            }
        } else if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            values.push(value.to_string());
        }
    }
    values
}

fn values_for_setting_path(args: &[String], setting_path: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        let (raw, json): (Option<&str>, bool) = if arg == "--setting" {
            (iter.next().map(String::as_str), false)
        } else if arg == "--setting-json" {
            (iter.next().map(String::as_str), true)
        } else if let Some(raw) = arg.strip_prefix("--setting=") {
            (Some(raw), false)
        } else if let Some(raw) = arg.strip_prefix("--setting-json=") {
            (Some(raw), true)
        } else {
            continue;
        };
        if let Some(raw) = raw {
            collect_setting_path_values(raw, setting_path, json, &mut values);
        }
    }
    values
}

fn collect_setting_path_values(
    raw: &str,
    setting_path: &str,
    json: bool,
    values: &mut Vec<String>,
) {
    let Some((key, value)) = raw.split_once('=') else {
        return;
    };
    if key == setting_path {
        values.push(value.to_string());
        return;
    }
    let Some(suffix) = setting_path
        .strip_prefix(key)
        .and_then(|suffix| suffix.strip_prefix('.'))
    else {
        return;
    };
    if !json {
        return;
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(value) else {
        return;
    };
    for segment in suffix.split('.') {
        let Some(next) = value.get(segment).cloned() else {
            return;
        };
        value = next;
    }
    collect_json_string_values(&value, values);
}

fn collect_json_string_values(value: &serde_json::Value, values: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => values.push(text.to_string()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_string_values(item, values);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_json_string_values(item, values);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_combines_provider_settings_and_rig_passthrough_inputs_before_staging() {
        let root = tempfile::tempdir().expect("workspace root");
        let primary = root.path().join("primary");
        let provider = root.path().join("provider");
        let fixture = root.path().join("fixture");
        std::fs::create_dir_all(primary.join("rigs/fixture-matrix")).expect("rig dir");
        std::fs::create_dir_all(&provider).expect("provider dir");
        std::fs::create_dir_all(&fixture).expect("fixture dir");
        std::fs::write(
            primary.join("rigs/fixture-matrix/rig.json"),
            r#"{"bench":{"path_inputs":["--fixture-root"]}}"#,
        )
        .expect("rig spec");

        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--rig".to_string(),
            "fixture-matrix".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({"provider_root": provider}).to_string(),
            "--setting".to_string(),
            format!("fixture_path={}", fixture.display()),
            "--".to_string(),
            "--fixture-root".to_string(),
            fixture.display().to_string(),
        ];
        let mut contract = LabOffloadCommand {
            hot_label: "bench",
            portable: true,
            unsupported_reason: None,
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            secret_env_sources: Vec::new(),
            required_extensions: Vec::new(),
            required_capabilities: Vec::new(),
            workload: None,
            routing_policy: crate::command_contract::LabRoutingPolicy::default(),
        };
        contract.workload = Some(crate::command_contract::LabRigWorkloadArguments {
            kind: crate::command_contract::LabRigWorkloadKind::Bench,
            rig_ids: vec!["fixture-matrix".to_string()],
            component: None,
            extension_overrides: Vec::new(),
        });

        let planner = PathMaterializationPlanner::plan(&args, &contract, &primary, false)
            .expect("materialization plan");
        let paths = planner
            .extra_workspaces
            .iter()
            .map(|workspace| workspace.path.clone())
            .collect::<Vec<_>>();

        assert!(paths.contains(&provider.canonicalize().expect("provider path")));
        assert!(paths.contains(&fixture.canonicalize().expect("fixture path")));
    }
}
