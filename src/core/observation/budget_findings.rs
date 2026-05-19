use crate::core::budget::BudgetFinding;

use super::records::NewFindingRecord;

fn finding_record_from_budget(run_id: &str, finding: &BudgetFinding) -> NewFindingRecord {
    NewFindingRecord {
        run_id: run_id.to_string(),
        tool: "budget".to_string(),
        rule: Some(finding.code.clone()),
        file: finding.file.clone(),
        line: None,
        severity: Some(finding.severity.clone()),
        fingerprint: Some(finding.fingerprint()),
        message: finding.message.clone(),
        fixable: None,
        metadata_json: serde_json::json!({
            "category": finding.category,
            "context_label": finding.context_label,
            "actual": finding.actual,
            "expected": finding.expected,
            "unit": finding.unit,
            "subject": finding.subject,
            "passed": finding.passed,
            "raw": finding,
        }),
    }
}

pub fn finding_records_from_budget(
    run_id: &str,
    findings: &[BudgetFinding],
) -> Vec<NewFindingRecord> {
    findings
        .iter()
        .map(|finding| finding_record_from_budget(run_id, finding))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let record = finding_record_from_budget("run-1", &finding);

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
        )];

        let records = finding_records_from_budget("run-1", &findings);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tool, "budget");
        assert_eq!(records[0].rule.as_deref(), Some("page.ready_ms"));
        assert_eq!(records[0].metadata_json["unit"], "ms");
    }
}
