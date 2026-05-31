use crate::core::finding::HomeboyFinding;

use super::records::NewFindingRecord;

fn finding_record_from_budget(run_id: &str, finding: &HomeboyFinding) -> NewFindingRecord {
    NewFindingRecord::from_homeboy_finding(run_id, finding.clone())
}

pub fn finding_records_from_budget(
    run_id: &str,
    findings: &[HomeboyFinding],
) -> Vec<NewFindingRecord> {
    findings
        .iter()
        .map(|finding| finding_record_from_budget(run_id, finding))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::budget::BudgetFinding;

    #[test]
    fn test_finding_record_from_budget() {
        let finding = BudgetFinding::failure(
            "rest.max_response_bytes",
            "profile:wordpress-rest",
            "REST response exceeded 250 KB budget",
            4378195.0,
            250000.0,
            "bytes",
            Some("/wp-json/datamachine/v1/pipelines?per_page=100".to_string()),
        );

        let normalized = finding.to_homeboy_finding();
        let record = finding_record_from_budget("run-1", &normalized);

        assert_eq!(record.tool, "budget");
        assert_eq!(record.rule.as_deref(), Some("rest.max_response_bytes"));
        assert_eq!(record.severity.as_deref(), Some("error"));
        assert_eq!(record.metadata_json["actual"], 4378195.0);
        assert_eq!(
            record.fingerprint.as_deref(),
            Some("rest.max_response_bytes:/wp-json/datamachine/v1/pipelines?per_page=100")
        );
    }

    #[test]
    fn test_finding_records_from_budget() {
        let findings = vec![BudgetFinding::failure(
            "page.ready_ms",
            "profile:page-ready",
            "Page ready time exceeded budget",
            1200.0,
            1000.0,
            "ms",
            Some("front-page".to_string()),
        )
        .to_homeboy_finding()];

        let records = finding_records_from_budget("run-1", &findings);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool, "budget");
        assert_eq!(records[0].rule.as_deref(), Some("page.ready_ms"));
        assert_eq!(records[0].metadata_json["unit"], "ms");
    }
}
