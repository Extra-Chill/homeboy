//! git_output — extracted from operations.rs.

use serde::{Deserialize, Serialize};


#[derive(Debug, Clone, Serialize)]

pub struct GitOutput {
    pub component_id: String,
    pub path: String,
    pub action: String,
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}
