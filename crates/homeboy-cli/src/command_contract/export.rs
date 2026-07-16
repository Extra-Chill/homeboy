//! Contract export document writing.
//!
//! Boundary: the `contract export` command builds the export documents from the
//! command-contract registry; this module owns the filesystem orchestration —
//! creating the output directory and writing each document as pretty JSON. It
//! takes plain data (file name + JSON value) so no command types are involved.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// A single contract export document to write.
pub struct ContractExportDocument {
    pub file_name: &'static str,
    pub schema: &'static str,
    pub description: &'static str,
    pub value: Value,
}

/// A written contract export file.
pub struct WrittenContractExport {
    pub path: PathBuf,
    pub schema: &'static str,
    pub description: &'static str,
}

/// Create `dir` and write each export document into it as pretty JSON with a
/// trailing newline. Returns the written files in input order.
pub fn write_contract_export_documents(
    dir: &Path,
    documents: Vec<ContractExportDocument>,
) -> crate::core::Result<Vec<WrittenContractExport>> {
    std::fs::create_dir_all(dir).map_err(|error| io_error(error, dir))?;

    let mut written = Vec::new();
    for document in documents {
        let path = dir.join(document.file_name);
        let body = serde_json::to_string_pretty(&document.value).map_err(json_error)?;
        std::fs::write(&path, format!("{body}\n")).map_err(|error| io_error(error, &path))?;
        written.push(WrittenContractExport {
            path,
            schema: document.schema,
            description: document.description,
        });
    }
    Ok(written)
}

fn io_error(error: std::io::Error, path: &Path) -> crate::core::Error {
    crate::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
}

fn json_error(error: serde_json::Error) -> crate::core::Error {
    crate::core::Error::internal_json(error.to_string(), Some("export contracts".to_string()))
}
