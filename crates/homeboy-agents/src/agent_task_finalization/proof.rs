use super::AgentTaskPrFinalizationOptions;
use homeboy_core::gate::HomeboyGateResult;
use homeboy_core::proof::{
    HomeboyProof, HomeboyProofArtifactRef, HomeboyProofEnvironmentDisposition,
    HomeboyProofEnvironmentVariable, HomeboyProofProvenance,
};

pub(super) fn build_finalization_proof(
    options: &AgentTaskPrFinalizationOptions,
    gates: Vec<HomeboyGateResult>,
) -> HomeboyProof {
    let provenance = HomeboyProofProvenance::homeboy_run(options.run_id.clone())
        .source_refs(options.evidence.source_refs.clone());
    let artifacts = options
        .evidence
        .artifact_refs
        .iter()
        .cloned()
        .map(HomeboyProofArtifactRef::uri);
    let environment = proof_environment_from_gates(&gates);

    HomeboyProof::new(
        format!("agent-task-finalization:{}", options.run_id),
        provenance,
    )
    .gates_requiring_ci_equivalent(gates)
    .artifacts(artifacts)
    .environment(environment)
}

fn proof_environment_from_gates(
    gates: &[HomeboyGateResult],
) -> Vec<HomeboyProofEnvironmentVariable> {
    let mut environment = Vec::new();
    for gate in gates {
        let Some(gate_environment) = gate.evidence.get("environment") else {
            continue;
        };
        environment.extend(proof_environment_variables(
            gate_environment.get("inherited"),
            HomeboyProofEnvironmentDisposition::Inherited,
        ));
        environment.extend(proof_environment_variables(
            gate_environment.get("sanitized"),
            HomeboyProofEnvironmentDisposition::Sanitized,
        ));
    }
    environment.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.value.cmp(&right.value))
            .then(format!("{:?}", left.disposition).cmp(&format!("{:?}", right.disposition)))
    });
    environment.dedup();
    environment
}

fn proof_environment_variables(
    variables: Option<&serde_json::Value>,
    disposition: HomeboyProofEnvironmentDisposition,
) -> Vec<HomeboyProofEnvironmentVariable> {
    variables
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|variable| {
            let name = variable.get("name")?.as_str()?;
            let value = variable.get("value")?.as_str()?;
            Some(HomeboyProofEnvironmentVariable {
                name: name.to_string(),
                value: value.to_string(),
                disposition,
            })
        })
        .collect()
}
