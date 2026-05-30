use crate::core::engine::run_dir;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum StructuredSidecarContract {
    Enabled(bool),
    Detail(StructuredSidecarDetail),
}

impl StructuredSidecarContract {
    pub(super) fn declaration(&self, name: &str) -> Option<StructuredSidecarDeclaration> {
        match self {
            StructuredSidecarContract::Enabled(true) => Some(StructuredSidecarDeclaration {
                name: name.to_string(),
                path: default_structured_sidecar_path(name),
                schema_version: None,
            }),
            StructuredSidecarContract::Enabled(false) => None,
            StructuredSidecarContract::Detail(detail) => {
                if !detail.enabled {
                    return None;
                }

                Some(StructuredSidecarDeclaration {
                    name: name.to_string(),
                    path: detail
                        .path
                        .clone()
                        .unwrap_or_else(|| default_structured_sidecar_path(name)),
                    schema_version: detail.schema_version.clone(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuredSidecarDetail {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_structured_sidecar_path(name: &str) -> String {
    match name {
        "lint.findings" => run_dir::files::LINT_FINDINGS,
        "test.results" => run_dir::files::TEST_RESULTS,
        "test.failures" => run_dir::files::TEST_FAILURES,
        "test.coverage" => run_dir::files::COVERAGE,
        "bench.results" => run_dir::files::BENCH_RESULTS,
        "annotations" => run_dir::files::ANNOTATIONS_DIR,
        _ => name,
    }
    .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuredSidecarDeclaration {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
}
