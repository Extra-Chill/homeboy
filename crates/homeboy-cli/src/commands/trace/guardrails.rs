use homeboy::core::extension::trace as extension_trace;
use homeboy::core::rig;

use super::{
    load_rig_context, required_trace_scenario, resolve_component_id, trace_variants_for_args,
    trace_workload_scenario_id, TraceArgs,
};

pub(super) fn run_trace_guardrails_for_args(
    args: &TraceArgs,
) -> homeboy::core::Result<Vec<extension_trace::TraceGuardrailOutput>> {
    let Some(context) = load_rig_context(args.rig.as_deref())? else {
        return Ok(Vec::new());
    };
    let scenario = required_trace_scenario(args)?;
    let component_id = resolve_component_id(&args.comp, Some(&context.rig_spec))?;
    let mut guardrails: Vec<(String, rig::TraceGuardrailSpec)> = Vec::new();

    guardrails.extend(
        context
            .rig_spec
            .trace_guardrails
            .iter()
            .cloned()
            .map(|guardrail| (format!("rig:{}", context.rig_spec.id), guardrail)),
    );

    for workload in context
        .rig_spec
        .trace_workloads
        .values()
        .flat_map(|workloads| workloads.iter())
    {
        if trace_workload_scenario_id(workload.path()) == scenario {
            guardrails.extend(
                workload
                    .trace_guardrails()
                    .iter()
                    .cloned()
                    .map(|guardrail| (format!("workload:{}", workload.path()), guardrail)),
            );
        }
    }

    let variants = trace_variants_for_args(&context, &component_id, &scenario);
    for variant_name in &args.variants {
        if let Some(variant) = variants.get(variant_name) {
            guardrails.extend(
                variant
                    .trace_guardrails
                    .iter()
                    .cloned()
                    .map(|guardrail| (format!("variant:{}", variant_name), guardrail)),
            );
        }
    }

    Ok(guardrails
        .into_iter()
        .enumerate()
        .map(|(index, (source, guardrail))| {
            let label = guardrail
                .label
                .clone()
                .unwrap_or_else(|| format!("guardrail-{}", index + 1));
            match rig::check::evaluate(&context.rig_spec, &guardrail.check) {
                Ok(()) => extension_trace::TraceGuardrailOutput {
                    label,
                    source,
                    passed: true,
                    status: "pass".to_string(),
                    failure: None,
                },
                Err(error) => extension_trace::TraceGuardrailOutput {
                    label,
                    source,
                    passed: false,
                    status: "fail".to_string(),
                    failure: Some(error.message),
                },
            }
        })
        .collect())
}
