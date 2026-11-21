//! Durable operator audit events. Automated services must not call this primitive.
use super::*;
use homeboy_control_plane_contract::*;
use rusqlite::params;

/// Record one completed operator mutation on a run or synthetic workflow/runner subject.
pub fn record_intervention(
    store: &ObservationStore,
    target: &str,
    command: &str,
    reason: Option<&str>,
) -> Result<()> {
    let run = RunId::new(target.to_string())
        .map_err(|error| Error::internal_unexpected(error.to_string()))?;
    let time = chrono::Utc::now().to_rfc3339();
    let actor = std::env::var("HOMEBOY_ACTOR").unwrap_or_else(|_| {
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
        let host = std::fs::read_to_string("/etc/hostname").unwrap_or_else(|_| "unknown".into());
        format!("{user}@{}", host.trim())
    });
    // Workflow instances and runners do not necessarily have observation run rows.
    if store.get_run(target)?.is_none() {
        store.connection.execute("INSERT OR IGNORE INTO runs(id, kind, started_at, status) VALUES (?1, 'intervention_subject', ?2, 'succeeded')", params![target, time])
            .map_err(sqlite_error("create intervention subject"))?;
    }
    let key = uuid::Uuid::new_v4().to_string();
    let request = ControlPlaneEventAppendRequest {
        schema: CONTROL_PLANE_EVENT_APPEND_REQUEST_SCHEMA.into(),
        idempotency_key: key.clone(),
        actor: actor.clone(),
        kind: "operator.intervention".into(),
        source: ControlPlaneEventSource {
            component: "homeboy-cli".into(),
            instance: None,
        },
        occurred_at: Some(time.clone()),
        task: None,
        attempt: None,
        execution: None,
        data: serde_json::json!({"time": time, "actor": actor, "command": command, "target": target, "reason": reason}),
        artifacts: vec![],
        evidence: vec![],
    };
    use sha2::Digest;
    let digest = format!("{:x}", sha2::Sha256::digest(key.as_bytes()));
    store.append_control_plane_event(&run, &request, &digest, &digest)?;
    Ok(())
}

impl ObservationStore {
    /// Global, bounded, newest-first audit projection, including pruned progress streams.
    pub fn interventions(&self, since: Option<&str>, limit: i64) -> Result<Vec<serde_json::Value>> {
        let exists: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'control_plane_event_appends')", [], |row| row.get(0)).map_err(sqlite_error("inspect intervention ledger"))?;
        if !exists {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare("SELECT json_extract(durable_event_json, '$.data') FROM control_plane_event_appends WHERE json_extract(durable_event_json, '$.kind') = 'operator.intervention' AND (?1 IS NULL OR created_at >= ?1) ORDER BY created_at DESC, rowid DESC LIMIT ?2")
            .map_err(sqlite_error("list interventions"))?;
        let rows = statement
            .query_map(params![since, limit.clamp(1, 1000)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_error("list interventions"))?;
        rows.map(|row| {
            let json = row.map_err(sqlite_error("read intervention"))?;
            serde_json::from_str(&json)
                .map_err(|error| Error::internal_unexpected(error.to_string()))
        })
        .collect()
    }
}
