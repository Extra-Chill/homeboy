//! types — extracted from mod.rs.

pub struct BuildOutput {
    pub command: String,
    pub component_id: String,
    pub build_command: String,
    #[serde(flatten)]
    pub output: CapturedOutput,
    pub success: bool,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum BuildResult {
    Single(BuildOutput),
    Bulk(BulkResult<BuildOutput>),
}
