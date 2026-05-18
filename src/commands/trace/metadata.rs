use super::*;

pub(super) fn trace_span_metadata_for_args(
    args: &TraceArgs,
) -> homeboy::core::Result<BTreeMap<String, extension_trace::TraceSpanMetadata>> {
    let Some(context) = load_rig_context(args.rig.as_deref())? else {
        return Ok(BTreeMap::new());
    };
    let effective_id = resolve_component_id(&args.comp, Some(&context.rig_spec))?;
    let path_override = args
        .comp
        .path
        .clone()
        .or_else(|| rig_component_path(&context.rig_spec, &effective_id));
    let component_override = rig_component_for_trace(&context.rig_spec, &effective_id);
    let ctx = execution_context::resolve_with_component(
        &ResolveOptions::with_capability_and_json(
            &effective_id,
            path_override,
            ExtensionCapability::Trace,
            args.setting_args.setting.clone(),
            args.setting_args.setting_json.clone(),
        ),
        component_override,
    )?;
    let Some(extension_id) = ctx.extension_id.as_deref() else {
        return Ok(BTreeMap::new());
    };
    let scenario = trace_scenario(args)?;
    let workloads = context
        .rig_spec
        .trace_workloads
        .get(extension_id)
        .map(|workloads| workloads.as_slice())
        .unwrap_or(&[]);
    let Some(workload) = workloads
        .iter()
        .find(|workload| trace_workload_scenario_id(workload.path()) == scenario)
    else {
        return Ok(BTreeMap::new());
    };
    Ok(workload
        .trace_span_metadata()
        .into_iter()
        .flat_map(|metadata| metadata.iter())
        .map(|(id, metadata)| (id.clone(), metadata.clone()))
        .collect())
}
