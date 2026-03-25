//! default — extracted from mod.rs.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;


pub(crate) fn is_default_remote(s: &str) -> bool {
    s == "origin"
}

pub(crate) fn is_default_branch(s: &str) -> bool {
    s == "main"
}
