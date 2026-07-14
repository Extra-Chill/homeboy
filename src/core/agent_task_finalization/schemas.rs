use super::{
    AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA, AGENT_TASK_PUBLICATION_INTENT_SCHEMA,
    AGENT_TASK_PUBLICATION_PROOF_SCHEMA,
};

pub(super) fn finalization_outcome_schema() -> String {
    AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA.to_string()
}

pub(super) fn publication_intent_schema() -> String {
    AGENT_TASK_PUBLICATION_INTENT_SCHEMA.to_string()
}

pub(super) fn publication_proof_schema() -> String {
    AGENT_TASK_PUBLICATION_PROOF_SCHEMA.to_string()
}
