use homeboy::core::rig;

use super::{trace_scenario, trace_workload_scenario_id, TraceArgs, TraceRigContext};

pub(super) fn trace_public_preview_for_args(
    args: &TraceArgs,
    rig_context: Option<&TraceRigContext>,
    extension_id: Option<&str>,
) -> homeboy::core::Result<Option<rig::TracePublicPreviewSpec>> {
    let Some(context) = rig_context else {
        return Ok(None);
    };

    if let Some(profile_id) = args.profile.as_deref() {
        if let Some(profile) = context.rig_spec.trace_profiles.get(profile_id) {
            if let Some(spec) = profile.public_preview.as_ref() {
                return Ok(Some(expand_trace_public_preview(context, spec)));
            }
        }
    }

    let Some(extension_id) = extension_id else {
        return Ok(None);
    };
    let scenario = trace_scenario(args)?;
    let Some(workload) = context
        .rig_spec
        .trace_workloads
        .get(extension_id)
        .and_then(|workloads| {
            workloads
                .iter()
                .find(|workload| trace_workload_scenario_id(workload.path()) == scenario)
        })
    else {
        return Ok(None);
    };

    Ok(workload
        .public_preview()
        .map(|spec| expand_trace_public_preview(context, spec)))
}

fn expand_trace_public_preview(
    context: &TraceRigContext,
    spec: &rig::TracePublicPreviewSpec,
) -> rig::TracePublicPreviewSpec {
    let expand = |value: &str| {
        let expanded = rig::expand::expand_vars(&context.rig_spec, value);
        match context.rig_package_root.as_ref() {
            Some(root) => expanded.replace("${package.root}", &root.to_string_lossy()),
            None => expanded,
        }
    };
    rig::TracePublicPreviewSpec {
        local_origin: expand(&spec.local_origin),
        public_origin: spec.public_origin.as_deref().map(&expand),
        command: spec.command.as_deref().map(expand),
        require_https: spec.require_https,
        provider: spec.provider.clone(),
        startup_timeout_seconds: spec.startup_timeout_seconds,
    }
}
