//! error — extracted from mod.rs.

use serde_json::Value;
use serde::{Deserialize, Serialize};
use crate::core::error::Hint;
use crate::core::error::ErrorCode;


#[derive(Debug, Clone)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
    pub hints: Vec<Hint>,
    pub retryable: Option<bool>,
}
