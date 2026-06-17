use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use super::*;

impl ObservationStore {
    pub(crate) fn record_triage_items(
        &self,
        items: &[NewTriageItemRecord],
    ) -> Result<Vec<TriageItemRecord>> {
        let mut records = Vec::with_capacity(items.len());
        for item in items {
            records.push(self.record_triage_item(item)?);
        }
        Ok(records)
    }

    fn record_triage_item(&self, item: &NewTriageItemRecord) -> Result<TriageItemRecord> {
        validate_required("triage_item.run_id", &item.run_id)?;
        validate_required("triage_item.provider", &item.provider)?;
        validate_required("triage_item.repo_owner", &item.repo_owner)?;
        validate_required("triage_item.repo_name", &item.repo_name)?;
        validate_required("triage_item.item_type", &item.item_type)?;
        validate_required("triage_item.state", &item.state)?;
        validate_required("triage_item.title", &item.title)?;
        validate_required("triage_item.url", &item.url)?;
        if self.get_run(&item.run_id)?.is_none() {
            return Err(Error::validation_invalid_argument(
                "triage_item.run_id",
                format!("referenced run record not found: {}", item.run_id),
                Some(item.run_id.clone()),
                None,
            ));
        }

        let id = Uuid::new_v4().to_string();
        let observed_at = chrono::Utc::now().to_rfc3339();
        let metadata_json = serialize_metadata(&item.metadata_json)?;
        let number = i64::try_from(item.number).map_err(|_| {
            Error::validation_invalid_argument(
                "triage_item.number",
                "number is too large to store in SQLite INTEGER",
                Some(item.number.to_string()),
                None,
            )
        })?;

        execute_with_retry("insert triage item record", || {
            self.connection.execute(
                r#"
                INSERT INTO triage_items(
                    id, run_id, provider, repo_owner, repo_name, item_type, number, state,
                    title, url, checks, review_decision, merge_state, next_action,
                    comments_count, reviews_count, last_comment_at, last_review_at, updated_at,
                    metadata_json, observed_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                "#,
                params![
                    id,
                    item.run_id,
                    item.provider,
                    item.repo_owner,
                    item.repo_name,
                    item.item_type,
                    number,
                    item.state,
                    item.title,
                    item.url,
                    item.signals.checks,
                    item.signals.review_decision,
                    item.signals.merge_state,
                    item.signals.next_action,
                    item.signals.comments_count,
                    item.signals.reviews_count,
                    item.signals.last_comment_at,
                    item.signals.last_review_at,
                    item.updated_at,
                    metadata_json,
                    observed_at,
                ],
            )
        })?;

        self.get_triage_item(&id)?.ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Inserted triage item record {id} but could not read it back"
            ))
        })
    }

    fn get_triage_item(&self, item_id: &str) -> Result<Option<TriageItemRecord>> {
        validate_required("triage_item_id", item_id)?;
        self.connection
            .query_row(
                r#"
                SELECT id, run_id, provider, repo_owner, repo_name, item_type, number, state,
                       title, url, checks, review_decision, merge_state, next_action,
                       comments_count, reviews_count, last_comment_at, last_review_at, updated_at,
                       metadata_json, observed_at
                FROM triage_items
                WHERE id = ?1
                "#,
                [item_id],
                row_to_triage_item_record,
            )
            .optional()
            .map_err(sqlite_error("read triage item record"))
    }

    pub(crate) fn list_triage_items_for_run(&self, run_id: &str) -> Result<Vec<TriageItemRecord>> {
        validate_required("run_id", run_id)?;
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, run_id, provider, repo_owner, repo_name, item_type, number, state,
                       title, url, checks, review_decision, merge_state, next_action,
                       comments_count, reviews_count, last_comment_at, last_review_at, updated_at,
                       metadata_json, observed_at
                FROM triage_items
                WHERE run_id = ?1
                ORDER BY provider ASC, repo_owner ASC, repo_name ASC, item_type ASC, number ASC
                "#,
            )
            .map_err(sqlite_error("prepare list run triage item records"))?;
        let rows = statement
            .query_map([run_id], row_to_triage_item_record)
            .map_err(sqlite_error("list run triage item records"))?;

        collect_rows(rows, "collect run triage item records")
    }
}

fn row_to_triage_item_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TriageItemRecord> {
    let number: i64 = row.get(6)?;
    Ok(TriageItemRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        provider: row.get(2)?,
        repo_owner: row.get(3)?,
        repo_name: row.get(4)?,
        item_type: row.get(5)?,
        number: u64::try_from(number).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Integer,
                Box::new(e),
            )
        })?,
        state: row.get(7)?,
        title: row.get(8)?,
        url: row.get(9)?,
        signals: TriagePullRequestSignals {
            checks: row.get(10)?,
            review_decision: row.get(11)?,
            merge_state: row.get(12)?,
            next_action: row.get(13)?,
            comments_count: row.get(14)?,
            reviews_count: row.get(15)?,
            last_comment_at: row.get(16)?,
            last_review_at: row.get(17)?,
        },
        updated_at: row.get(18)?,
        metadata_json: parse_metadata(row.get(19)?)?,
        observed_at: row.get(20)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::NewRunRecord;
    use crate::test_support::with_isolated_home;

    #[test]
    fn test_record_triage_item() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = start_triage_run(&store);
            let record = store
                .record_triage_item(&sample_triage_item(&run.id))
                .expect("triage item");

            assert_eq!(record.signals.comments_count, Some(3));
            assert_eq!(record.signals.reviews_count, Some(2));
        });
    }

    #[test]
    fn test_record_triage_items() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = start_triage_run(&store);

            let records = store
                .record_triage_items(&[sample_triage_item(&run.id)])
                .expect("triage items");

            assert_eq!(records.len(), 1);
            assert_eq!(records[0].signals.comments_count, Some(3));
            assert_eq!(records[0].signals.reviews_count, Some(2));
        });
    }

    #[test]
    fn test_list_triage_items_for_run() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = start_triage_run(&store);
            let records = store
                .record_triage_items(&[sample_triage_item(&run.id)])
                .expect("triage items");

            let listed = store.list_triage_items_for_run(&run.id).expect("listed");

            assert_eq!(listed, records);
        });
    }

    fn start_triage_run(store: &ObservationStore) -> RunRecord {
        store
            .start_run(
                NewRunRecord::builder("triage")
                    .component_id("workspace")
                    .command("triage.workspace")
                    .homeboy_version("test")
                    .build(),
            )
            .expect("run")
    }

    fn sample_triage_item(run_id: &str) -> NewTriageItemRecord {
        NewTriageItemRecord {
            run_id: run_id.to_string(),
            provider: "github".to_string(),
            repo_owner: "Extra-Chill".to_string(),
            repo_name: "homeboy".to_string(),
            item_type: "pull_request".to_string(),
            number: 42,
            state: "OPEN".to_string(),
            title: "Add triage observations".to_string(),
            url: "https://github.com/Extra-Chill/homeboy/pull/42".to_string(),
            signals: TriagePullRequestSignals {
                checks: Some("SUCCESS".to_string()),
                review_decision: Some("REVIEW_REQUIRED".to_string()),
                merge_state: Some("CLEAN".to_string()),
                next_action: Some("review_required".to_string()),
                comments_count: Some(3),
                reviews_count: Some(2),
                last_comment_at: Some("2026-05-08T10:00:00Z".to_string()),
                last_review_at: Some("2026-05-08T11:00:00Z".to_string()),
            },
            updated_at: Some("2026-05-08T12:00:00Z".to_string()),
            metadata_json: serde_json::json!({ "labels": ["enhancement"] }),
        }
    }
}
