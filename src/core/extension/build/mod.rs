mod internal_implementation;
mod public_api;
mod resolved_build_command;
mod types;

pub use internal_implementation::*;
pub use public_api::*;
pub use resolved_build_command::*;
pub use types::*;

}

// === Public API ===

// === Internal implementation ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_json_input_detects_json() {
        assert!(is_json_input(r#"{"componentIds": ["a"]}"#));
        assert!(is_json_input(r#"  {"componentIds": ["a"]}"#));
        assert!(!is_json_input("extrachill-api"));
        assert!(!is_json_input("some-component-id"));
    }
}
