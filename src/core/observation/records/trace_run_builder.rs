use super::NewTraceRunRecord;

#[derive(Debug, Clone)]
pub struct NewTraceRunRecordBuilder {
    record: NewTraceRunRecord,
}

impl NewTraceRunRecordBuilder {
    pub fn new(
        run_id: impl Into<String>,
        component_id: impl Into<String>,
        scenario_id: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            record: NewTraceRunRecord {
                run_id: run_id.into(),
                component_id: component_id.into(),
                rig_id: None,
                scenario_id: scenario_id.into(),
                status: status.into(),
                baseline_status: None,
                metadata_json: serde_json::json!({}),
            },
        }
    }

    pub fn trace_rig_id(mut self, rig_id: Option<impl Into<String>>) -> Self {
        self.record.rig_id = rig_id.map(Into::into);
        self
    }

    pub fn baseline_status(mut self, baseline_status: Option<impl Into<String>>) -> Self {
        self.record.baseline_status = baseline_status.map(Into::into);
        self
    }

    pub fn metadata(mut self, metadata_json: serde_json::Value) -> Self {
        self.record.metadata_json = metadata_json;
        self
    }

    pub fn build(self) -> NewTraceRunRecord {
        self.record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let record = NewTraceRunRecordBuilder::new("run-1", "homeboy", "scenario", "pass").build();

        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.component_id, "homeboy");
        assert_eq!(record.scenario_id, "scenario");
        assert_eq!(record.status, "pass");
        assert!(record.rig_id.is_none());
        assert!(record.baseline_status.is_none());
        assert_eq!(record.metadata_json, serde_json::json!({}));
    }

    #[test]
    fn test_trace_rig_id() {
        let record = NewTraceRunRecordBuilder::new("run-1", "homeboy", "scenario", "pass")
            .trace_rig_id(Some("studio"))
            .build();

        assert_eq!(record.rig_id.as_deref(), Some("studio"));
    }

    #[test]
    fn test_baseline_status() {
        let record = NewTraceRunRecordBuilder::new("run-1", "homeboy", "scenario", "pass")
            .baseline_status(Some("pass"))
            .build();

        assert_eq!(record.baseline_status.as_deref(), Some("pass"));
    }

    #[test]
    fn test_metadata() {
        let metadata = serde_json::json!({ "span_count": 1 });
        let record = NewTraceRunRecordBuilder::new("run-1", "homeboy", "scenario", "pass")
            .metadata(metadata.clone())
            .build();

        assert_eq!(record.metadata_json, metadata);
    }
}
