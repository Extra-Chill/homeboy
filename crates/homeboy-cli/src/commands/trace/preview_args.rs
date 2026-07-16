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
        mode: spec.mode.clone(),
        local_origin: expand(&spec.local_origin),
        public_origin: spec.public_origin.as_deref().map(&expand),
        command: spec.command.as_deref().map(expand),
        require_https: spec.require_https,
        provider: spec.provider.clone(),
        startup_timeout_seconds: spec.startup_timeout_seconds,
        required_asset_paths: spec
            .required_asset_paths
            .iter()
            .map(|path| expand(path))
            .collect(),
        asset_fanout: spec
            .asset_fanout
            .as_ref()
            .map(|fanout| rig::TracePreviewAssetFanoutSpec {
                asset_paths: fanout.asset_paths.iter().map(|path| expand(path)).collect(),
                concurrency: fanout.concurrency,
                repeat_count: fanout.repeat_count,
                expected_body_contains: fanout.expected_body_contains.as_deref().map(expand),
            }),
        native: spec
            .native
            .as_ref()
            .map(|native| rig::TraceNativePublicPreviewSpec {
                public_host: native.public_host.as_deref().map(&expand),
                operator_domain: native.operator_domain.as_deref().map(&expand),
                session_id: native.session_id.as_deref().map(&expand),
                ingress_url: native.ingress_url.as_deref().map(&expand),
                token_env: native.token_env.clone(),
                client_binary: native.client_binary.as_deref().map(&expand),
            }),
    }
}
