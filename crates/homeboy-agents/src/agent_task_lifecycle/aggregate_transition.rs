use super::*;

/// The authoritative transition from a controller aggregate to its durable run
/// record. Callers retain ownership of source-specific validation and metadata;
/// this layer owns the shared record, aggregate, workspace, and artifact
/// projections.
pub(crate) struct AgentTaskAggregateTransition<'a> {
    pub record: &'a mut AgentTaskRunRecord,
    pub plan: &'a AgentTaskPlan,
    pub aggregate: &'a AgentTaskAggregate,
}

pub(crate) fn apply_aggregate_transition_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    transition: AgentTaskAggregateTransition<'_>,
) -> Result<AgentTaskRunRecord> {
    let AgentTaskAggregateTransition {
        record,
        plan,
        aggregate,
    } = transition;
    let aggregate_path = lifecycle_store
        .aggregate_path(&record.run_id)
        .display()
        .to_string();
    apply_aggregate_to_record(record, plan, aggregate, aggregate_path);
    lifecycle_store.write_aggregate_and_record(record, aggregate)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, record, aggregate)?;
    Ok(record.clone())
}
