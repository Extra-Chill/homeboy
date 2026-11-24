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
