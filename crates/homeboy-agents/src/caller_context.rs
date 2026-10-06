//! Opaque caller ownership captured before task transport or detachment.
use homeboy_core::{Error, Result};
use serde_json::{json, Value};

pub fn capture(context: Value) -> Result<Value> {
    capture_with(
        context,
        std::env::var("HOMEBOY_CALLER_CONTEXT").ok().as_deref(),
    )
}

/// A child execution keeps the controller-owned checkout and original caller,
/// independently of the worker's execution directory or ambient shell owner.
pub fn inherit_ownership(target: &mut Value, source: &Value) {
    for field in ["client_context", "caller_workspace"] {
        if target.get(field).is_none() {
            if let Some(value) = source.get(field) {
                target[field] = value.clone();
            }
        }
    }
}

pub fn capture_with(mut context: Value, caller: Option<&str>) -> Result<Value> {
    if context.is_null() {
        context = json!({});
    }
    let object = context.as_object_mut().ok_or_else(|| {
        Error::validation_invalid_argument(
            "client_context",
            "Caller context must be a JSON object",
            None,
            None,
        )
    })?;
    if !object.contains_key("caller_context") {
        if let Some(caller) = caller.filter(|value| !value.trim().is_empty()) {
            if caller.len() > 512 {
                return Err(Error::validation_invalid_argument(
                    "caller_context",
                    "Caller reference exceeds 512 bytes",
                    None,
                    None,
                ));
            }
            object.insert("caller_context".into(), json!(caller));
        }
    }
    if let Some(caller) = object.get("caller_context") {
        if !caller
            .as_str()
            .is_some_and(|value| !value.is_empty() && value.len() <= 512)
        {
            return Err(Error::validation_invalid_argument(
                "caller_context",
                "Use a non-empty opaque caller reference of at most 512 bytes",
                None,
                None,
            ));
        }
    }
    Ok(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_scope_caller_capture_preserves_original_owner() {
        let first = capture_with(json!({"other": true}), Some("source-A")).unwrap();
        assert_eq!(first["caller_context"], "source-A");
        assert_eq!(capture_with(first.clone(), Some("retry-B")).unwrap(), first);
        assert_eq!(capture_with(json!({}), None).unwrap(), json!({}));
        assert!(capture_with(json!({"caller_context": 12}), None).is_err());
    }
}
