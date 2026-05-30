use crate::core::extension::test::{
    FailureCategory, FailureCluster, TestAnalysisInput, TestFailure,
};
use crate::core::finding::{FindingSource, HomeboyFinding};

use super::records::NewFindingRecord;

pub(crate) fn homeboy_finding_from_test_failure(failure: &TestFailure) -> HomeboyFinding {
    let mut normalized = HomeboyFinding::builder("test", test_failure_message(failure))
        .severity("error")
        .fingerprint(test_failure_fingerprint(failure))
        .source(FindingSource::new("sidecar").label("test-failures"))
        .metadata("record_kind", "failure")
        .metadata("source_sidecar", "test-failures")
        .metadata("test_name", failure.test_name.clone())
        .metadata("test_file", failure.test_file.clone())
        .metadata("source_file", failure.source_file.clone())
        .metadata("source_line", failure.source_line)
        .raw(failure)
        .build();
    normalized.rule = non_empty_string(&failure.error_type);
    normalized.location.file =
        non_empty_string(&failure.test_file).or_else(|| non_empty_string(&failure.source_file));
    normalized.location.line = (failure.source_line > 0).then_some(i64::from(failure.source_line));
    normalized
}

pub(crate) fn homeboy_findings_from_test_analysis_input(
    input: &TestAnalysisInput,
) -> Option<Vec<HomeboyFinding>> {
    if input.failures.is_empty() {
        return None;
    }

    Some(
        input
            .failures
            .iter()
            .map(homeboy_finding_from_test_failure)
            .collect(),
    )
}

fn finding_record_from_test_failure(run_id: &str, failure: &TestFailure) -> NewFindingRecord {
    NewFindingRecord::from_homeboy_finding(run_id, homeboy_finding_from_test_failure(failure))
}

pub(crate) fn finding_records_from_test_analysis_input(
    run_id: &str,
    input: &TestAnalysisInput,
) -> Vec<NewFindingRecord> {
    input
        .failures
        .iter()
        .map(|failure| finding_record_from_test_failure(run_id, failure))
        .collect()
}

fn homeboy_finding_from_failure_cluster(cluster: &FailureCluster) -> HomeboyFinding {
    let category = failure_category_slug(&cluster.category);
    let mut normalized = HomeboyFinding::builder("test", cluster.pattern.clone())
        .rule(format!("cluster:{category}"))
        .category(category)
        .severity("error")
        .fingerprint(format!("test-cluster::{}", cluster.id))
        .source(FindingSource::new("analysis").label("test-failure-cluster"))
        .metadata("record_kind", "analysis_cluster")
        .metadata("cluster_id", cluster.id.clone())
        .metadata("count", cluster.count)
        .metadata("affected_files", cluster.affected_files.clone())
        .metadata("example_tests", cluster.example_tests.clone())
        .metadata("suggested_fix", cluster.suggested_fix.clone())
        .raw(cluster)
        .build();
    normalized.fix.fixable = cluster.suggested_fix.is_some().then_some(true);
    normalized
}

fn finding_record_from_failure_cluster(run_id: &str, cluster: &FailureCluster) -> NewFindingRecord {
    NewFindingRecord::from_homeboy_finding(run_id, homeboy_finding_from_failure_cluster(cluster))
}

pub(crate) fn finding_records_from_failure_clusters(
    run_id: &str,
    clusters: &[FailureCluster],
) -> Vec<NewFindingRecord> {
    clusters
        .iter()
        .map(|cluster| finding_record_from_failure_cluster(run_id, cluster))
        .collect()
}

fn test_failure_message(failure: &TestFailure) -> String {
    if failure.error_type.is_empty() {
        failure.message.clone()
    } else if failure.message.is_empty() {
        failure.error_type.clone()
    } else {
        format!("{}: {}", failure.error_type, failure.message)
    }
}

fn test_failure_fingerprint(failure: &TestFailure) -> String {
    format!(
        "test::{}::{}::{}::{}",
        failure.test_file, failure.test_name, failure.error_type, failure.message
    )
}

fn failure_category_slug(category: &FailureCategory) -> &'static str {
    match category {
        FailureCategory::MissingMethod => "missing_method",
        FailureCategory::MissingClass => "missing_class",
        FailureCategory::ReturnTypeChange => "return_type_change",
        FailureCategory::ErrorCodeChange => "error_code_change",
        FailureCategory::AssertionMismatch => "assertion_mismatch",
        FailureCategory::MockError => "mock_error",
        FailureCategory::FatalError => "fatal_error",
        FailureCategory::SignatureChange => "signature_change",
        FailureCategory::EnvironmentError => "environment_error",
        FailureCategory::Other => "other",
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failure_projects_to_homeboy_finding() {
        let failure = TestFailure {
            test_name: "AuthTest::test_login".to_string(),
            test_file: "tests/AuthTest.php".to_string(),
            error_type: "AssertionFailedError".to_string(),
            message: "Expected 200, got 500".to_string(),
            source_file: "src/Auth.php".to_string(),
            source_line: 44,
        };

        let finding = homeboy_finding_from_test_failure(&failure);

        assert_eq!(finding.tool, "test");
        assert_eq!(finding.rule.as_deref(), Some("AssertionFailedError"));
        assert_eq!(finding.severity.as_deref(), Some("error"));
        assert_eq!(finding.location.file.as_deref(), Some("tests/AuthTest.php"));
        assert_eq!(finding.location.line, Some(44));
        assert_eq!(finding.metadata_json()["record_kind"], "failure");
        assert_eq!(finding.metadata_json()["source_line"], 44);
    }

    #[test]
    fn test_failure_record_uses_shared_projection() {
        let input = TestAnalysisInput {
            failures: vec![TestFailure {
                test_name: "AuthTest::test_login".to_string(),
                test_file: String::new(),
                error_type: String::new(),
                message: "Timed out".to_string(),
                source_file: "src/Auth.php".to_string(),
                source_line: 12,
            }],
            total: 1,
            passed: 0,
        };

        let records = finding_records_from_test_analysis_input("run-1", &input);

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run_id, "run-1");
        assert_eq!(records[0].message, "Timed out");
        assert_eq!(records[0].file.as_deref(), Some("src/Auth.php"));
        assert_eq!(records[0].metadata_json["source_sidecar"], "test-failures");
    }

    #[test]
    fn failure_cluster_projects_to_homeboy_finding() {
        let cluster = FailureCluster {
            id: "cluster-1".to_string(),
            pattern: "Missing method render()".to_string(),
            category: FailureCategory::MissingMethod,
            count: 3,
            affected_files: vec!["tests/ViewTest.php".to_string()],
            example_tests: vec!["ViewTest::test_render".to_string()],
            suggested_fix: Some("Add render()".to_string()),
        };

        let record = finding_record_from_failure_cluster("run-1", &cluster);

        assert_eq!(record.tool, "test");
        assert_eq!(record.rule.as_deref(), Some("cluster:missing_method"));
        assert_eq!(record.fixable, Some(true));
        assert_eq!(
            record.fingerprint.as_deref(),
            Some("test-cluster::cluster-1")
        );
        assert_eq!(record.metadata_json["category"], "missing_method");
        assert_eq!(record.metadata_json["count"], 3);
    }
}
