//! trait_impls — extracted from mod.rs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::core::error::std::fmt::Display for Error;
use crate::core::error::Result;
use crate::core::error::Error;


impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
