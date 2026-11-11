use crate::agent_task_lifecycle::AgentTaskRunRecord;

/// Homeboy-owned dispatch identity. Missing model receipts are valid provenance.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExecutionProvenance {
    pub tool: String,
    pub route: String,
    pub served_models: Vec<String>,
}

/// Tool and route come from the provider runtime Homeboy dispatched; served
/// models are the runtime's `responseModel` receipt recorded on the run.
pub fn execution_provenance(record: &AgentTaskRunRecord) -> ExecutionProvenance {
    let Some(runtime) = record.lifecycle.provider_runtime.last() else {
        return ExecutionProvenance::default();
    };
    ExecutionProvenance {
        tool: runtime.backend.clone(),
        route: runtime.metadata["route"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        served_models: record.served_models.clone(),
    }
}

pub fn render_execution_provenance(provenance: &ExecutionProvenance) -> String {
    if provenance.tool.is_empty() {
        return "operator-finalized; no Homeboy provider execution".into();
    }
    let tool = if provenance.tool.eq_ignore_ascii_case("pi") {
        "Pi"
    } else {
        &provenance.tool
    };
    let mut text = tool.to_string();
    if !provenance.route.is_empty() {
        text.push_str(&format!(" · route {}", provenance.route));
    }
    if !provenance.served_models.is_empty() {
        text.push_str(&format!(
            " · served {}",
            provenance.served_models.join(", ")
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_served_model_renders_with_dispatch_route() {
        assert_eq!(
            render_execution_provenance(&ExecutionProvenance {
                tool: "pi".into(),
                route: "cook-heavy".into(),
                served_models: vec!["openai/gpt-6.1-sol".into()],
            }),
            "Pi · route cook-heavy · served openai/gpt-6.1-sol"
        );
    }
}
