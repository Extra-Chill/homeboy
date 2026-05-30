use crate::core::budget::BudgetFinding;
use crate::core::finding::{FindingSource, HomeboyFinding};

use super::records::NewFindingRecord;

pub fn homeboy_finding_from_budget(finding: &BudgetFinding) -> HomeboyFinding {
    let mut normalized = HomeboyFinding::builder("budget", finding.message.clone())
        .rule(finding.code.clone())
        .category(finding.category.clone())
        .severity(finding.severity.clone())
        .fingerprint(finding.fingerprint())
        .source(FindingSource::new("budget").label(finding.context_label.clone()))
        .metadata("context_label", finding.context_label.clone())
        .metadata("actual", finding.actual)
        .metadata("expected", finding.expected)
        .metadata("unit", finding.unit.clone())
        .metadata("subject", finding.subject.clone())
        .metadata("passed", finding.passed)
        .raw(finding)
        .build();
    normalized.file = finding.file.clone();
    normalized
}

fn finding_record_from_budget(run_id: &str, finding: &BudgetFinding) -> NewFindingRecord {
    NewFindingRecord::from_homeboy_finding(run_id, homeboy_finding_from_budget(finding))
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
