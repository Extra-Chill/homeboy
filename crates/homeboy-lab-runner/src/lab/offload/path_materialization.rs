//! Controller-side discovery of every Lab path materialization input.
//!
//! The planner is deliberately evaluated before workspace mutation.  Staging
//! consumes its resolved argv and workspace list rather than rediscovering
//! inputs after individual argument transforms.

use super::*;
use homeboy_rig;

pub(crate) struct PathMaterializationPlanner {
    pub(crate) args: Vec<String>,
    pub(crate) extra_workspaces: Vec<ExtraLabWorkspace>,
}

impl PathMaterializationPlanner {
    pub(crate) fn plan(
        args: &[String],
        workload: Option<&homeboy_core::lab_contract::LabRigWorkloadArguments>,
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
        extra_workspaces.extend(path_values_extra_workspaces(
            path_setting_values(&args),
            source_path,
            "path_setting",
        )?);
        extra_workspaces.extend(rig_declared_path_input_extra_workspaces(
            &args,
            workload,
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
    workload: Option<&homeboy_core::lab_contract::LabRigWorkloadArguments>,
    primary_source_path: &Path,
) -> Result<Vec<ExtraLabWorkspace>> {
    if !workload.is_some_and(|workload| {
        matches!(
            workload.kind,
            homeboy_core::lab_contract::LabRigWorkloadKind::Bench
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

fn load_primary_rig_spec(
    primary_source_path: &Path,
    rig_id: &str,
) -> Result<Option<homeboy_rig::RigSpec>> {
    if !primary_source_path.join("rig.json").is_file() && !primary_source_path.join("rigs").is_dir()
    {
        return Ok(None);
    }
    let Some(discovered) = homeboy_rig::discover_rigs(primary_source_path)?
        .into_iter()
        .find(|candidate| candidate.id == rig_id)
    else {
        return Ok(None);
    };
    Ok(Some(homeboy_rig::load_local_source(
        &discovered.rig_path.to_string_lossy(),
        Some(discovered.id.as_str()),
    )?))
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
            command: homeboy_core::lab_contract::LabCommandContract::portable(
                "bench",
                None,
                false,
                &[],
            ),
            required_extensions: Vec::new(),
            required_capabilities: Vec::new(),
            workload: None,
        };
        contract.workload = Some(homeboy_core::lab_contract::LabRigWorkloadArguments {
            kind: homeboy_core::lab_contract::LabRigWorkloadKind::Bench,
            rig_ids: vec!["fixture-matrix".to_string()],
            component: None,
            extension_overrides: Vec::new(),
        });

        let planner =
            PathMaterializationPlanner::plan(&args, contract.workload.as_ref(), &primary, false)
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
