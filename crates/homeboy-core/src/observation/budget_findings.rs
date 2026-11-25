use crate::finding::HomeboyFinding;

use super::records::{finding_records_from_homeboy_findings, NewFindingRecord};

pub fn finding_records_from_budget(
    run_id: &str,
    findings: &[HomeboyFinding],
) -> Vec<NewFindingRecord> {
    finding_records_from_homeboy_findings(run_id, findings.iter().cloned())
}
