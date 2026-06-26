use std::collections::HashMap;
use std::path::Path;

use homeboy::core::component::{Component, ScopedExtensionConfig};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::engine::invocation::InvocationRequirements;
use homeboy::core::extension::{self, ExtensionCapability};
use homeboy::core::fuzz::{
    merge_fuzz_target_inventory, parse_fuzz_target_inventory_file, FuzzTargetInventory,
    FUZZ_CONTRACT_VERSION, FUZZ_TARGET_INVENTORY_SCHEMA,
};
use homeboy::core::rig::{self, RigSpec};

use super::super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs};
use super::report::fuzz_provenance;
use super::types::FuzzWorkloadOutput;

pub(super) type FuzzRigContext = rig::RigSourceContext;

pub(super) fn load_rig(
    rig_id: Option<&str>,
    settings: &SettingArgs,
) -> homeboy::core::Result<Option<FuzzRigContext>> {
    let Some(rig_id) = rig_id else {
        return Ok(None);
    };
    let mut context = rig::RigSourceContext::load(rig_id)?;
    apply_fuzz_rig_setting_overrides(&mut context.spec, settings)?;
    Ok(Some(context))
}

fn apply_fuzz_rig_setting_overrides(
    spec: &mut RigSpec,
    settings: &SettingArgs,
) -> homeboy::core::Result<()> {
    if settings.setting.is_empty() && settings.setting_json.is_empty() {
        return Ok(());
    }

    let mut value = serde_json::to_value(&*spec).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "failed to encode fuzz rig spec for setting overrides: {error}"
        ))
    })?;
    for (key, raw) in &settings.setting {
        apply_dotted_json_override(&mut value, key, serde_json::Value::String(raw.clone()));
    }
    for (key, raw) in &settings.setting_json {
        apply_dotted_json_override(&mut value, key, raw.clone());
    }
    *spec = serde_json::from_value(value).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "setting",
            format!("fuzz rig setting overrides produced an invalid rig spec: {error}"),
            None,
            None,
        )
    })?;
    Ok(())
}

fn apply_dotted_json_override(target: &mut serde_json::Value, key: &str, value: serde_json::Value) {
    let parts = key
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return;
    }
    let mut current = target;
    for part in &parts[..parts.len() - 1] {
        if !current.is_object() {
            *current = serde_json::Value::Object(serde_json::Map::new());
        }
        current = current
            .as_object_mut()
            .expect("current setting target is object")
            .entry((*part).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
    if !current.is_object() {
        *current = serde_json::Value::Object(serde_json::Map::new());
    }
    current
        .as_object_mut()
        .expect("setting target is object")
        .insert(parts[parts.len() - 1].to_string(), value);
}

pub(super) fn resolve_component_id(
    comp: &PositionalComponentArgs,
    rig_spec: Option<&RigSpec>,
) -> homeboy::core::Result<String> {
    if let Some(id) = comp.id() {
        return Ok(id.to_string());
    }

    if let Some(spec) = rig_spec {
        if let Some(default) = spec
            .fuzz
            .as_ref()
            .and_then(|fuzz| fuzz.default_component.as_deref())
        {
            return Ok(default.to_string());
        }

        return Err(homeboy::core::Error::validation_invalid_argument(
            "fuzz.default_component",
            format!(
                "rig '{}' does not declare fuzz.default_component; pass a component id or add fuzz.default_component to the rig spec",
                spec.id
            ),
            None,
            None,
        ));
    }

    comp.resolve_id()
}

pub(super) fn resolve_fuzz_context(
    component_id: &str,
    comp: &PositionalComponentArgs,
    settings: &SettingArgs,
    extension_override: &ExtensionOverrideArgs,
    capability: ExtensionCapability,
    rig_context: Option<&FuzzRigContext>,
) -> homeboy::core::Result<execution_context::ExecutionContext> {
    let rig_spec = rig_context.map(|context| &context.spec);
    let path_override = comp
        .path
        .clone()
        .or_else(|| rig_spec.and_then(|spec| rig_component_path(spec, component_id)));
    let component_override = rig_spec.and_then(|spec| rig_component_for_fuzz(spec, component_id));

    let mut resolve_options = ResolveOptions::with_capability_and_json(
        component_id,
        path_override,
        capability,
        settings.setting.clone(),
        settings.setting_json.clone(),
    );
    resolve_options.extension_overrides = extension_override.extensions.clone();

    execution_context::resolve_with_component(&resolve_options, component_override)
}

fn rig_component_path(spec: &RigSpec, component_id: &str) -> Option<String> {
    rig::resolve_component_path(spec, component_id).ok()
}

pub(super) fn rig_component_for_fuzz(spec: &RigSpec, component_id: &str) -> Option<Component> {
    let rig_component = spec.components.get(component_id)?;
    let mut extensions = rig_component.extensions.clone()?;
    expand_rig_extension_settings(spec, &mut extensions);
    let mut component = rig::resolve_component(spec, component_id).ok()?;
    component.remote_url = rig_component.remote_url.clone().or(component.remote_url);
    component.extensions = Some(extensions);
    component.resolve_remote_path();
    Some(component)
}

fn expand_rig_extension_settings(
    spec: &RigSpec,
    extensions: &mut HashMap<String, ScopedExtensionConfig>,
) {
    for extension in extensions.values_mut() {
        for value in extension.settings.values_mut() {
            expand_rig_setting_value(spec, value);
        }
    }
}

fn expand_rig_setting_value(spec: &RigSpec, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(raw) => {
            *raw = rig::expand::expand_vars(spec, raw);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                expand_rig_setting_value(spec, value);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values_mut() {
                expand_rig_setting_value(spec, value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzz_rig_setting_overrides_update_component_extension_context() {
        let mut spec: RigSpec = serde_json::from_value(serde_json::json!({
            "id": "package-fuzz",
            "components": {
                "package": {
                    "path": "/workspace/stale/plugins/package",
                    "branch": "main",
                    "extensions": {
                        "generic-runtime": {
                            "source_root": "/workspace/stale",
                            "source_subpath": "packages/component"
                        }
                    }
                }
            }
        }))
        .expect("parse rig spec");
        let settings = SettingArgs {
            setting: vec![
                (
                    "components.package.path".to_string(),
                    "/workspace/current/plugins/package".to_string(),
                ),
                (
                    "components.package.extensions.generic-runtime.source_root".to_string(),
                    "/workspace/current".to_string(),
                ),
            ],
            setting_json: vec![],
        };

        apply_fuzz_rig_setting_overrides(&mut spec, &settings).expect("apply overrides");

        let component = spec.components.get("package").expect("component");
        assert_eq!(component.path, "/workspace/current/plugins/package");
        assert_eq!(
            component
                .extensions
                .as_ref()
                .and_then(|extensions| extensions.get("generic-runtime"))
                .and_then(|extension| extension.settings.get("source_root"))
                .and_then(serde_json::Value::as_str),
            Some("/workspace/current")
        );
    }
}

pub(super) fn fuzz_workloads(
    component: &homeboy::core::component::Component,
    rig_context: Option<&FuzzRigContext>,
    extension_id: Option<&str>,
) -> Vec<FuzzWorkloadOutput> {
    let mut workloads: Vec<FuzzWorkloadOutput> = component
        .script_commands(ExtensionCapability::Fuzz)
        .iter()
        .enumerate()
        .map(|(index, _command)| FuzzWorkloadOutput {
            id: format!("component-script-{}", index + 1),
            label: None,
            description: None,
            source: "component.scripts.fuzz".to_string(),
            manifest_path: None,
        })
        .collect();

    if let Some(extension_id) = extension_id {
        workloads.extend(
            fuzz_rig_workload_inputs(rig_context, Some(extension_id))
                .workload_paths
                .into_iter()
                .map(|path| fuzz_workload_from_path(extension_id, &path)),
        );
    }

    if let Some(extensions) = component.extensions.as_ref() {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = extension::load_extension(extension_id) {
                workloads.extend(manifest.fuzz_workloads().iter().map(|workload| {
                    FuzzWorkloadOutput {
                        id: workload.id.clone(),
                        label: workload.label.clone(),
                        description: workload.description.clone(),
                        source: format!("extension:{extension_id}"),
                        manifest_path: None,
                    }
                }));
            }
        }
    }

    workloads
}

pub(super) fn fuzz_invocation_requirements(
    rig_context: Option<&FuzzRigContext>,
    extension_id: Option<&str>,
) -> InvocationRequirements {
    fuzz_rig_workload_inputs(rig_context, extension_id).invocation_requirements
}

fn fuzz_rig_workload_inputs(
    rig_context: Option<&FuzzRigContext>,
    extension_id: Option<&str>,
) -> rig::RigExtensionWorkloadInputs {
    let Some((context, extension_id)) = rig_context.zip(extension_id) else {
        return rig::RigExtensionWorkloadInputs {
            workload_paths: Vec::new(),
            invocation_requirements: InvocationRequirements::default(),
        };
    };

    rig::extension_workload_inputs(
        &context.spec,
        rig::RigWorkloadKind::Fuzz,
        context.package_root.as_deref(),
        extension_id,
    )
}

fn fuzz_workload_from_path(extension_id: &str, path: &Path) -> FuzzWorkloadOutput {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("rig-fuzz-workload")
        .to_string();
    FuzzWorkloadOutput {
        id,
        label: path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
        description: None,
        source: format!("rig_workloads:{extension_id}:{}", path.to_string_lossy()),
        manifest_path: Some(path.to_string_lossy().to_string()),
    }
}

pub(super) fn select_workload<'a>(
    workloads: &'a [FuzzWorkloadOutput],
    workload_id: Option<&str>,
) -> homeboy::core::Result<Option<&'a FuzzWorkloadOutput>> {
    if let Some(workload_id) = workload_id {
        return workloads
            .iter()
            .find(|workload| workload.id == workload_id)
            .map(Some)
            .ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "workload",
                    format!("Unknown fuzz workload '{workload_id}'. Run `homeboy fuzz list` to inspect declared workloads."),
                    None,
                    None,
                )
            });
    }

    if workloads.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workload",
            "No fuzz workloads are declared for this component/rig/extension selection",
            None,
            None,
        )
        .with_hint("Run `homeboy fuzz list <component> --rig <id>` to inspect the resolved selection.")
        .with_hint("Declare extension fuzz workloads, component scripts.fuzz commands, or rig fuzz_workloads before claiming fuzz coverage.")
        .with_hint("If the command is available in source but not on the Lab runner, run `homeboy runner status <id>` and refresh or upgrade the runner binary."));
    }

    let mut path_workloads = workloads
        .iter()
        .filter(|workload| workload.manifest_path.is_some());
    let first = path_workloads.next();
    if first.is_some() && path_workloads.next().is_none() {
        return Ok(first);
    }

    if workloads.len() > 1 {
        let workload_ids = workloads
            .iter()
            .map(|workload| workload.id.clone())
            .collect::<Vec<_>>();
        return Err(homeboy::core::Error::validation_invalid_argument(
            "workload",
            "Multiple fuzz workloads are declared; select one explicitly with --workload <id>",
            None,
            None,
        )
        .with_hint(format!(
            "Available workload ids: {}",
            workload_ids.join(", ")
        ))
        .with_hint(
            "Run `homeboy fuzz list` for labels, descriptions, sources, and manifest paths.",
        ));
    }

    Ok(None)
}

pub(super) fn build_target_inventory(
    component_id: &str,
    workloads: &[FuzzWorkloadOutput],
    run_id: Option<String>,
    inventory_path: Option<&Path>,
) -> homeboy::core::Result<FuzzTargetInventory> {
    let mut inventory = FuzzTargetInventory {
        schema: FUZZ_TARGET_INVENTORY_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: format!("{}-inventory", component_id),
        surfaces: Vec::new(),
        targets: Vec::new(),
        workloads: Vec::new(),
        seeds: Vec::new(),
        provenance: Some(fuzz_provenance(run_id)),
        metadata: serde_json::json!({
            "declared_workloads": workloads,
        }),
        extra: std::collections::BTreeMap::new(),
    };

    if let Some(path) = inventory_path {
        let discovered = parse_fuzz_target_inventory_file(path)?;
        inventory.metadata["inventory_file"] =
            serde_json::Value::String(path.to_string_lossy().to_string());
        merge_fuzz_target_inventory(&mut inventory, discovered);
    }

    Ok(inventory)
}
