use super::NewTraceSpanRecord;

#[derive(Debug, Clone)]
pub struct NewTraceSpanRecordBuilder {
    record: NewTraceSpanRecord,
}

impl NewTraceSpanRecordBuilder {
    pub fn new(
        run_id: impl Into<String>,
        span_id: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            record: NewTraceSpanRecord {
                run_id: run_id.into(),
                span_id: span_id.into(),
                status: status.into(),
                duration_ms: None,
                from_event: None,
                to_event: None,
                metadata_json: serde_json::json!({}),
            },
        }
    }

    pub fn duration_ms(mut self, duration_ms: Option<f64>) -> Self {
        self.record.duration_ms = duration_ms;
        self
    }

    pub fn from_event(mut self, from_event: Option<impl Into<String>>) -> Self {
        self.record.from_event = from_event.map(Into::into);
        self
    }

    pub fn to_event(mut self, to_event: Option<impl Into<String>>) -> Self {
        self.record.to_event = to_event.map(Into::into);
        self
    }

    pub fn metadata(mut self, metadata_json: serde_json::Value) -> Self {
        self.record.metadata_json = metadata_json;
        self
    }

    pub fn build(self) -> NewTraceSpanRecord {
        self.record
    }
}

impl NewTraceSpanRecord {
    pub fn builder(
        run_id: impl Into<String>,
        span_id: impl Into<String>,
        status: impl Into<String>,
    ) -> NewTraceSpanRecordBuilder {
        NewTraceSpanRecordBuilder::new(run_id, span_id, status)
    }
}
