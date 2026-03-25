//! helpers — extracted from types.rs.

use crate::config;
use crate::error::Result;
use serde::Serialize;
use crate::component::Component;


/// Parse bulk component IDs from a JSON spec.
pub fn parse_bulk_component_ids(json_spec: &str) -> Result<Vec<String>> {
    let input = config::parse_bulk_ids(json_spec)?;
    Ok(input.component_ids)
}
