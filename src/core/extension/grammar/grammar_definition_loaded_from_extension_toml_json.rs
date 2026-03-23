//! grammar_definition_loaded_from_extension_toml_json — extracted from grammar.rs.

use super::default;

pub(crate) fn default_fallback_default() -> String {
    "Default::default()".to_string()
}

pub(crate) fn default_group_1() -> usize {
    1
}

pub(crate) fn default_group_2() -> usize {
    2
}

pub(crate) fn default_return_type_separator() -> String {
    "->".to_string()
}

pub(crate) fn default_param_format() -> String {
    "name_colon_type".to_string()
}

pub(crate) fn default_quotes() -> Vec<String> {
    vec!["\"".to_string(), "'".to_string()]
}

pub(crate) fn default_escape_string() -> String {
    "\\".to_string()
}

pub(crate) fn default_open() -> String {
    "{".to_string()
}

pub(crate) fn default_close() -> String {
    "}".to_string()
}

pub(crate) fn default_context() -> String {
    "any".to_string()
}

pub(crate) fn default_true() -> bool {
    true
}
