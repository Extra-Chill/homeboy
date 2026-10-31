//! Diagnostic identity of the controller executing a cook.

use serde_json::{json, Value};

pub const CONTROLLER_RUNTIME_METADATA_KEY: &str = "controller_runtime";

pub fn current() -> Value {
    let identity = crate::build_identity::current().display;
    json!({
        "originating": { "build_identity": identity },
    })
}
