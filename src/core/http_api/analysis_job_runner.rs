use serde::Serialize;
use serde_json::Value;

use crate::core::error::{Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct AnalysisJobRunOutput {
    pub exit_code: i32,
    pub output: Value,
}

pub trait AnalysisJobRunner: Clone + Send + 'static {
    fn run_analysis_job(&self, argv: Vec<String>) -> Result<AnalysisJobRunOutput>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedAnalysisJobRunner;

impl AnalysisJobRunner for UnsupportedAnalysisJobRunner {
    fn run_analysis_job(&self, _argv: Vec<String>) -> Result<AnalysisJobRunOutput> {
        Err(Error::validation_invalid_argument(
            "analysis_runner",
            "analysis job runner is not configured for this HTTP API entrypoint",
            None,
            Some(vec![
                "Run analysis job endpoints through the daemon command adapter".to_string(),
            ]),
        ))
    }
}
