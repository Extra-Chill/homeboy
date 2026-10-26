//! Opaque caller ownership captured before task transport or detachment.
use homeboy_core::{Error, Result};
use serde_json::{json, Value};

pub fn capture(context: Value) -> Result<Value> {
    capture_with_requester(
        context,
        std::env::var("HOMEBOY_CALLER_CONTEXT").ok().as_deref(),
        std::env::var("HOMEBOY_REQUESTER").ok().as_deref(),
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

pub fn capture_with(context: Value, caller: Option<&str>) -> Result<Value> {
    capture_with_requester(context, caller, None)
}

pub fn capture_with_requester(
    mut context: Value,
    caller: Option<&str>,
    requester: Option<&str>,
) -> Result<Value> {
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
    if !object.contains_key("requester") {
        if let Some(requester) = requester.filter(|value| !value.is_empty()) {
            object.insert("requester".into(), json!(requester));
        }
    }
    if let Some(requester) = object.get("requester") {
        if !requester.as_str().is_some_and(|value| {
            !value.is_empty() && value.len() <= 512 && !value.contains(['\r', '\n'])
        }) {
            return Err(Error::validation_invalid_argument(
                "requester",
                "Use a single-line opaque requester of at most 512 bytes",
                None,
                None,
            ));
        }
    }
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
    fn captures_generic_requester_environment_without_caller() {
        let _lock = homeboy_core::test_support::env_lock();
        let _requester =
            homeboy_core::test_support::EnvVarGuard::set("HOMEBOY_REQUESTER", "opaque-user:42");
        let _caller = homeboy_core::test_support::EnvVarGuard::unset("HOMEBOY_CALLER_CONTEXT");
        let context = capture(json!({})).unwrap();
        assert_eq!(context["requester"], "opaque-user:42");
        assert!(context.get("caller_context").is_none());
    }

    #[test]
    fn requester_survives_serialization_and_worker_handoff() {
        let context =
            capture_with_requester(json!({}), Some("owner"), Some("opaque-user:42")).unwrap();
        let source = json!({"client_context": context});
        let serialized = serde_json::to_string(&source).unwrap();
        let source: Value = serde_json::from_str(&serialized).unwrap();
        let mut worker = json!({});
        inherit_ownership(&mut worker, &source);
        let recaptured = capture_with_requester(
            worker["client_context"].clone(),
            Some("worker"),
            Some("different-user"),
        )
        .unwrap();
        assert_eq!(recaptured["requester"], "opaque-user:42");
        assert_eq!(recaptured["caller_context"], "owner");
    }

    #[test]
    fn active_scope_caller_capture_preserves_original_owner() {
        let first = capture_with(json!({"other": true}), Some("source-A")).unwrap();
        assert_eq!(first["caller_context"], "source-A");
        assert_eq!(capture_with(first.clone(), Some("retry-B")).unwrap(), first);
        assert_eq!(capture_with(json!({}), None).unwrap(), json!({}));
        assert!(capture_with(json!({"caller_context": 12}), None).is_err());
    }
}
