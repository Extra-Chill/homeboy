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
    let decided_from = record.state;
    apply_aggregate_to_record(record, plan, aggregate, aggregate_path);
    // Compare-and-swap at the revision this record was read at: a stale
    // projection can no longer replace a terminal decision it never saw (#15718).
    lifecycle_store.project_terminal_aggregate(record, decided_from, aggregate)?;
    *record = lifecycle_store.read_record(&record.run_id)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, record, aggregate)?;
    Ok(record.clone())
}
