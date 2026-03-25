//! error — extracted from mod.rs.

use serde_json::Value;
use serde::{Deserialize, Serialize};
use crate::core::error::ErrorCode;
use crate::core::error::Result;
use crate::core::error::Hint;


#[derive(Debug, Clone)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
    pub hints: Vec<Hint>,
    pub retryable: Option<bool>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}
