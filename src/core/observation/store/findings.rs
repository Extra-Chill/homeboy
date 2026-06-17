use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::*;

impl ObservationStore {
    pub fn record_findings(&self, findings: &[NewFindingRecord]) -> Result<Vec<FindingRecord>> {
        if findings.is_empty() {
            return Ok(Vec::new());
        }

        let run_id = &findings[0].run_id;
        validate_required("finding.run_id", run_id)?;
        if self.get_run(run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "finding.run_id",
                format!("referenced run record not found: {}", run_id),
                Some(run_id.clone()),
                None,
            ));
        }
        for finding in findings {
            validate_required("finding.run_id", &finding.run_id)?;
            if finding.run_id != *run_id {
                return Err(Error::validation_invalid_argument(
                    "finding.run_id",
                    "all batch findings must reference the same run record".to_string(),
                    Some(finding.run_id.clone()),
                    None,
                ));
            }
            validate_required("finding.tool", &finding.tool)?;
            validate_required("finding.message", &finding.message)?;
        }

        self.connection
            .execute_batch("BEGIN IMMEDIATE TRANSACTION")
            .map_err(sqlite_error("begin finding batch"))?;

        let mut records = Vec::with_capacity(findings.len());
        for finding in findings {
            match self.insert_finding_unchecked(finding) {
                Ok(record) => records.push(record),
                Err(error) => {
                    let _ = self.connection.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
        }

        self.connection
            .execute_batch("COMMIT")
            .map_err(sqlite_error("commit finding batch"))?;
        Ok(records)
    }

    pub fn record_finding(&self, finding: &NewFindingRecord) -> Result<FindingRecord> {
        validate_required("finding.run_id", &finding.run_id)?;
        validate_required("finding.tool", &finding.tool)?;
        validate_required("finding.message", &finding.message)?;
        if self.get_run(&finding.run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "finding.run_id",
                format!("referenced run record not found: {}", finding.run_id),
                Some(finding.run_id.clone()),
                None,
            ));
        }

        self.insert_finding_unchecked(finding)
    }

    fn insert_finding_unchecked(&self, finding: &NewFindingRecord) -> Result<FindingRecord> {
        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let metadata_json = serialize_metadata(&finding.metadata_json)?;
        let fixable = finding
            .fixable
            .map(|value| if value { 1_i64 } else { 0_i64 });

        execute_with_retry("insert finding record", || {
            self.connection.execute(
                r#"
                INSERT INTO findings(
                    id, run_id, tool, rule, file, line, severity, fingerprint, message,
                    fixable, metadata_json, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    id,
                    finding.run_id,
                    finding.tool,
                    finding.rule,
                    finding.file,
                    finding.line,
                    finding.severity,
                    finding.fingerprint,
                    finding.message,
                    fixable,
                    metadata_json,
                    created_at,
                ],
            )
        })?;

        Ok(FindingRecord {
            id,
            run_id: finding.run_id.clone(),
            tool: finding.tool.clone(),
            rule: finding.rule.clone(),
            file: finding.file.clone(),
            line: finding.line,
            severity: finding.severity.clone(),
            fingerprint: finding.fingerprint.clone(),
            message: finding.message.clone(),
            fixable: finding.fixable,
            metadata_json: finding.metadata_json.clone(),
            created_at,
        })
    }

    pub fn get_finding(&self, finding_id: &str) -> Result<Option<FindingRecord>> {
        validate_required("finding_id", finding_id)?;
        self.connection
            .query_row(
                r#"
                SELECT id, run_id, tool, rule, file, line, severity, fingerprint, message,
                       fixable, metadata_json, created_at
                FROM findings
                WHERE id = ?1
                "#,
                [finding_id],
                row_to_finding_record,
            )
            .optional()
            .map_err(sqlite_error("read finding record"))
    }

    pub fn import_finding(&self, finding: &FindingRecord) -> Result<()> {
        validate_required("finding.id", &finding.id)?;
        validate_required("finding.run_id", &finding.run_id)?;
        validate_required("finding.tool", &finding.tool)?;
        validate_required("finding.message", &finding.message)?;
        if self.get_run(&finding.run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "finding.run_id",
                format!("referenced run record not found: {}", finding.run_id),
                Some(finding.run_id.clone()),
                None,
            ));
        }
        if let Some(existing) = self.get_finding(&finding.id)? {
            return ensure_identical("finding", &finding.id, &existing, finding);
        }
        let metadata_json = serialize_metadata(&finding.metadata_json)?;
        let fixable = finding
            .fixable
            .map(|value| if value { 1_i64 } else { 0_i64 });
        execute_with_retry("import finding record", || {
            self.connection.execute(
                r#"
                INSERT INTO findings(
                    id, run_id, tool, rule, file, line, severity, fingerprint, message,
                    fixable, metadata_json, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                "#,
                params![
                    finding.id,
                    finding.run_id,
                    finding.tool,
                    finding.rule,
                    finding.file,
                    finding.line,
                    finding.severity,
                    finding.fingerprint,
                    finding.message,
                    fixable,
                    metadata_json,
                    finding.created_at,
                ],
            )
        })?;
        Ok(())
    }

    pub fn list_findings(&self, filter: FindingListFilter) -> Result<Vec<FindingRecord>> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1000);
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, run_id, tool, rule, file, line, severity, fingerprint, message,
                       fixable, metadata_json, created_at
                FROM findings
                WHERE (?1 IS NULL OR run_id = ?1)
                  AND (?2 IS NULL OR tool = ?2)
                  AND (?3 IS NULL OR file = ?3)
                  AND (?4 IS NULL OR fingerprint = ?4)
                ORDER BY created_at ASC, rowid ASC
                LIMIT ?5
                "#,
            )
            .map_err(sqlite_error("prepare list finding records"))?;
        let rows = statement
            .query_map(
                params![
                    filter.run_id.as_deref(),
                    filter.tool.as_deref(),
                    filter.file.as_deref(),
                    filter.fingerprint.as_deref(),
                    limit,
                ],
                row_to_finding_record,
            )
            .map_err(sqlite_error("list finding records"))?;

        collect_rows(rows, "collect finding records")
    }

    pub fn list_findings_for_run(&self, run_id: &str) -> Result<Vec<FindingRecord>> {
        validate_required("run_id", run_id)?;
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, run_id, tool, rule, file, line, severity, fingerprint, message,
                       fixable, metadata_json, created_at
                FROM findings
                WHERE run_id = ?1
                ORDER BY created_at ASC, rowid ASC
                "#,
            )
            .map_err(sqlite_error("prepare list run finding records"))?;
        let rows = statement
            .query_map([run_id], row_to_finding_record)
            .map_err(sqlite_error("list run finding records"))?;

        collect_rows(rows, "collect run finding records")
    }

    pub fn latest_finding(&self, filter: FindingListFilter) -> Result<Option<FindingRecord>> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, run_id, tool, rule, file, line, severity, fingerprint, message,
                       fixable, metadata_json, created_at
                FROM findings
                WHERE (?1 IS NULL OR run_id = ?1)
                  AND (?2 IS NULL OR tool = ?2)
                  AND (?3 IS NULL OR file = ?3)
                ORDER BY created_at DESC, rowid DESC
                LIMIT 1
                "#,
            )
            .map_err(sqlite_error("prepare latest finding record"))?;

        statement
            .query_row(
                params![
                    filter.run_id.as_deref(),
                    filter.tool.as_deref(),
                    filter.file.as_deref(),
                ],
                row_to_finding_record,
            )
            .optional()
            .map_err(sqlite_error("read latest finding record"))
    }
}

fn row_to_finding_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<FindingRecord> {
    let fixable: Option<i64> = row.get(9)?;
    Ok(FindingRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        tool: row.get(2)?,
        rule: row.get(3)?,
        file: row.get(4)?,
        line: row.get(5)?,
        severity: row.get(6)?,
        fingerprint: row.get(7)?,
        message: row.get(8)?,
        fixable: fixable.map(|value| value != 0),
        metadata_json: parse_metadata(row.get(10)?)?,
        created_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    struct XdgGuard(Option<String>);

    impl XdgGuard {
        fn unset() -> Self {
            let prior = std::env::var("XDG_DATA_HOME").ok();
            std::env::remove_var("XDG_DATA_HOME");
            Self(prior)
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    fn new_run() -> NewRunRecord {
        NewRunRecord::builder("lint")
            .component_id("homeboy")
            .command("homeboy lint")
            .cwd_path(std::path::Path::new("/tmp/homeboy"))
            .homeboy_version("test")
            .build()
    }

    fn new_finding(run_id: &str, rule: &str) -> NewFindingRecord {
        NewFindingRecord {
            run_id: run_id.to_string(),
            tool: "lint".to_string(),
            rule: Some(rule.to_string()),
            file: Some(format!("src/{rule}.rs")),
            line: Some(1),
            severity: Some("warning".to_string()),
            fingerprint: Some(format!("src/{rule}.rs::{rule}")),
            message: format!("{rule} finding"),
            fixable: Some(false),
            metadata_json: serde_json::json!({ "category": rule }),
        }
    }

    #[test]
    fn test_record_finding() {
        with_isolated_home(|_| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.start_run(new_run()).expect("start");

            let finding = store
                .record_finding(&new_finding(&run.id, "security"))
                .expect("finding");

            assert_eq!(finding.rule.as_deref(), Some("security"));
        });
    }

    #[test]
    fn test_record_findings() {
        with_isolated_home(|_| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.start_run(new_run()).expect("start");

            let findings = store
                .record_findings(&[
                    new_finding(&run.id, "security"),
                    new_finding(&run.id, "i18n"),
                ])
                .expect("findings");

            assert_eq!(findings.len(), 2);
            assert_eq!(findings[0].rule.as_deref(), Some("security"));
            assert_eq!(findings[1].rule.as_deref(), Some("i18n"));
        });
    }

    #[test]
    fn test_record_findings_rejects_unknown_run_before_insert() {
        with_isolated_home(|_| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");

            let result = store.record_findings(&[
                new_finding("missing-run", "security"),
                new_finding("missing-run", "i18n"),
            ]);

            assert!(result.is_err());
            let findings = store
                .list_findings(FindingListFilter::default())
                .expect("list");
            assert!(findings.is_empty());
        });
    }

    #[test]
    fn test_record_findings_requires_one_run_per_batch() {
        with_isolated_home(|_| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let first = store.start_run(new_run()).expect("first run");
            let second = store.start_run(new_run()).expect("second run");

            let result = store.record_findings(&[
                new_finding(&first.id, "security"),
                new_finding(&second.id, "i18n"),
            ]);

            assert!(result.is_err());
            let findings = store
                .list_findings(FindingListFilter::default())
                .expect("list");
            assert!(findings.is_empty());
        });
    }

    #[test]
    fn test_list_findings() {
        with_isolated_home(|_| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.start_run(new_run()).expect("start");
            store
                .record_findings(&[
                    new_finding(&run.id, "security"),
                    new_finding(&run.id, "i18n"),
                ])
                .expect("findings");

            let findings = store
                .list_findings(FindingListFilter {
                    run_id: Some(run.id),
                    tool: Some("lint".to_string()),
                    ..FindingListFilter::default()
                })
                .expect("list");

            assert_eq!(findings.len(), 2);
        });
    }

    #[test]
    fn test_latest_finding_uses_filters_and_deterministic_tie_break() {
        with_isolated_home(|_| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.start_run(new_run()).expect("start");
            let old = store
                .record_finding(&new_finding(&run.id, "security"))
                .expect("old finding");
            let latest = store
                .record_finding(&new_finding(&run.id, "security"))
                .expect("latest finding");
            store
                .record_finding(&new_finding(&run.id, "i18n"))
                .expect("other finding");

            let selected = store
                .latest_finding(FindingListFilter {
                    run_id: Some(run.id),
                    tool: Some("lint".to_string()),
                    file: Some("src/security.rs".to_string()),
                    ..FindingListFilter::default()
                })
                .expect("latest finding")
                .expect("finding exists");
            let missing = store
                .latest_finding(FindingListFilter {
                    file: Some("src/missing.rs".to_string()),
                    ..FindingListFilter::default()
                })
                .expect("missing latest");

            assert_eq!(selected.id, latest.id);
            assert_ne!(selected.id, old.id);
            assert!(missing.is_none());
        });
    }
}
