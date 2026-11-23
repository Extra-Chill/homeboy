use serde_json::Value;

pub(super) fn gates(request: &Value) -> Vec<&str> {
    request
        .pointer("/inputs/homeboy/gates/verify")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

pub(super) fn describe(request: &mut crate::agent_task::AgentTaskRequest) {
    let value = serde_json::to_value(&*request).expect("serializable request");
    if value.pointer("/inputs/homeboy/gates").is_none() {
        return;
    }
    request.instructions.push_str("\n\nHomeboy verification contract: Homeboy runs the following declared gates after this attempt. Do not repeat full verification in the provider session; keep your own checks narrow (for example, cargo check -p <crate> or one targeted test). Hand off the candidate for Homeboy's deterministic verification: these gates are Homeboy's to run after you finish, so their pending result never makes the task incomplete. Declare it complete when your change is ready and your narrow checks pass. Public gate commands (exact):\n");
    request
        .instructions
        .push_str(&serde_json::to_string(&gates(&value)).expect("serializable gates"));
    if let Some(count) = value
        .pointer("/inputs/homeboy/gates/private_verify_count")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
    {
        request.instructions.push_str(&format!(
            "\nHomeboy also runs {count} private gates; their commands are not disclosed."
        ));
    }
}

/// Classify shell command segments, not mentions in prose such as `echo cargo test`.
/// Compound commands are attributed once, with declared gates taking precedence.
pub(super) fn command_class(command: Option<&str>, gates: &[&str]) -> &'static str {
    let Some(command) = command else {
        return "unknown";
    };
    let normalize = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let segments: Vec<_> = command
        .split([';', '&', '|', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if gates.iter().any(|gate| {
        normalize(command) == normalize(gate)
            || segments
                .iter()
                .any(|segment| normalize(segment) == normalize(gate))
    }) {
        return "declared_gate";
    }
    if segments.iter().any(|segment| {
        let mut words = segment
            .split_whitespace()
            .skip_while(|word| *word == "env" || word.contains('='));
        let program = words
            .next()
            .unwrap_or_default()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        let action = words.find(|word| !word.starts_with('-'));
        matches!(
            (program, action),
            (
                "cargo",
                Some("build" | "check" | "test" | "clippy" | "bench" | "nextest")
            ) | ("npm" | "pnpm" | "yarn", Some("test" | "build"))
                | ("pytest" | "make", _)
        )
    }) {
        "build_test"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_contract_delivers_exact_public_gates_and_narrow_check_guidance() {
        let mut request: crate::agent_task::AgentTaskRequest = serde_json::from_value(serde_json::json!({
            "task_id": "contract", "executor": {"backend": "pi"}, "instructions": "Implement it.",
            "inputs": {"homeboy": {"gates": {"verify": ["custom-verify --exact 'a b'"], "private_verify_count": 1}}}
        })).unwrap();
        describe(&mut request);
        assert!(request.instructions.starts_with("Implement it."));
        assert!(request.instructions.contains("after this attempt"));
        assert!(request.instructions.contains("cargo check -p <crate>"));
        assert!(request.instructions.contains("never makes the task incomplete"));
        assert!(request.instructions.contains("custom-verify --exact 'a b'"));
        assert!(request.instructions.contains("1 private gates"));
    }
}
