//! deploy_result — extracted from types.rs.

use serde::Serialize;
use crate::component::Component;
use crate::error::Result;


pub struct DeployResult {
    pub success: bool,
    pub exit_code: i32,
    pub error: Option<String>,
}
