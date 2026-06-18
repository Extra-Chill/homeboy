use crate::core::finding::HomeboyFinding;

use super::records::{finding_records_from_homeboy_findings, NewFindingRecord};

#[cfg(test)]
fn finding_record_from_budget(run_id: &str, finding: &HomeboyFinding) -> NewFindingRecord {
    NewFindingRecord::from_homeboy_finding(run_id, finding.clone())
}

pub fn finding_records_from_budget(
    run_id: &str,
    findings: &[HomeboyFinding],
) -> Vec<NewFindingRecord> {
    finding_records_from_homeboy_findings(run_id, findings.iter().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::finding::{FindingSource, HomeboyFinding};

    fn budget_finding(
        code: &str,
        context_label: &str,
        message: &str,
        actual: f64,
        expected: f64,
        unit: &str,
        subject: Option<String>,
    ) -> HomeboyFinding {
        HomeboyFinding::builder("budget", message)
            .rule(code)
            .category("budget")
            .severity("error")
            .fingerprint(match subject.as_deref() {
                Some(subject) if !subject.is_empty() => format!("{}:{}", code, subject),
                _ => code.to_string(),
            })
            .source(FindingSource::new("budget").label(context_label))
            .metadata("actual", actual)
            .metadata("expected", expected)
            .metadata("unit", unit)
            .metadata("subject", subject)
            .build()
    }

    #[test]
    fn test_finding_record_from_budget() {
        let finding = budget_finding(
            "rest.max_response_bytes",
            "profile:wordpress-rest",
            "REST response exceeded 250 KB budget",
            4378195.0,
            250000.0,
            "bytes",
            Some("/wp-json/sampleplugin/v1/pipelines?per_page=100".to_string()),
        );

        let record = finding_record_from_budget("run-1", &finding);

        assert_eq!(record.tool, "budget");
        assert_eq!(record.rule.as_deref(), Some("rest.max_response_bytes"));
        assert_eq!(record.severity.as_deref(), Some("error"));
        assert_eq!(record.metadata_json["actual"], 4378195.0);
        assert_eq!(
            record.fingerprint.as_deref(),
            Some("rest.max_response_bytes:/wp-json/sampleplugin/v1/pipelines?per_page=100")
        );
    }

    #[test]
    fn test_finding_records_from_budget() {
        let findings = vec![budget_finding(
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
