//! Serialized, generated CLI reference contract.
//!
//! The runtime adapter projects its live Clap tree into this contract when the
//! command surface changes. Regular reference validation only needs this leaf
//! crate, while a separate runtime parity test proves the projection stays exact.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const CLI_REFERENCE_SCHEMA: &str = "homeboy/cli-reference/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliReference {
    pub schema: String,
    pub documents: BTreeMap<String, String>,
}

impl CliReference {
    pub fn new(documents: BTreeMap<String, String>) -> Self {
        Self {
            schema: CLI_REFERENCE_SCHEMA.to_string(),
            documents,
        }
    }
}

pub fn checked_in_cli_reference() -> CliReference {
    let reference: CliReference = serde_json::from_str(include_str!(
        "../../../../docs/reference/cli/command-surface.json"
    ))
    .expect("checked-in CLI reference contract must be valid JSON");
    assert_eq!(
        reference.schema, CLI_REFERENCE_SCHEMA,
        "unsupported CLI reference schema"
    );
    reference
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn checked_in_contract_matches_reference_docs() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("docs/reference/cli/commands");
        let reference = checked_in_cli_reference();
        let mut actual = BTreeMap::new();
        for entry in std::fs::read_dir(directory).expect("read CLI reference directory") {
            let path = entry.expect("read CLI reference entry").path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                actual.insert(
                    name,
                    std::fs::read_to_string(path).expect("read CLI reference"),
                );
            }
        }
        assert_eq!(actual, reference.documents);
    }
}
