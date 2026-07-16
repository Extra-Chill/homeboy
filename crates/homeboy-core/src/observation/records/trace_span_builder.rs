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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let record = NewTraceSpanRecordBuilder::new("run-1", "boot", "ok").build();

        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.span_id, "boot");
        assert_eq!(record.status, "ok");
        assert!(record.duration_ms.is_none());
        assert!(record.from_event.is_none());
        assert!(record.to_event.is_none());
        assert_eq!(record.metadata_json, serde_json::json!({}));
    }

    #[test]
    fn test_duration_ms() {
        let record = NewTraceSpanRecordBuilder::new("run-1", "boot", "ok")
            .duration_ms(Some(12.5))
            .build();

        assert_eq!(record.duration_ms, Some(12.5));
    }

    #[test]
    fn test_from_event() {
        let record = NewTraceSpanRecordBuilder::new("run-1", "boot", "ok")
            .from_event(Some("start"))
            .build();

        assert_eq!(record.from_event.as_deref(), Some("start"));
    }

    #[test]
    fn test_to_event() {
        let record = NewTraceSpanRecordBuilder::new("run-1", "boot", "ok")
            .to_event(Some("ready"))
            .build();

        assert_eq!(record.to_event.as_deref(), Some("ready"));
    }

    #[test]
    fn test_metadata() {
        let metadata = serde_json::json!({ "phase": "cold" });
        let record = NewTraceSpanRecordBuilder::new("run-1", "boot", "ok")
            .metadata(metadata.clone())
            .build();

        assert_eq!(record.metadata_json, metadata);
    }
}
