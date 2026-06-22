use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ConfigKeyUsageConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ConfigKeyUsageRule>,
}

impl ConfigKeyUsageConfig {
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub(super) fn merge(&mut self, other: &ConfigKeyUsageConfig) {
        for rule in &other.rules {
            if !self.rules.iter().any(|existing| existing.id == rule.id) {
                self.rules.push(rule.clone());
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigKeyUsageRule {
    /// Stable rule label used in finding descriptions and merge de-duplication.
    pub id: String,
    /// Optional path substrings excluded from all evidence collection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_path_contains: Vec<String>,
    /// Regexes that capture keys written or migrated into storage/builders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_patterns: Vec<ConfigKeyUsagePattern>,
    /// Regexes that capture accessors/backing helpers for keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accessor_patterns: Vec<ConfigKeyUsagePattern>,
    /// Regexes that capture non-test runtime/display reads of keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_patterns: Vec<ConfigKeyUsagePattern>,
    /// Optional regex templates that match references to accessor symbols.
    /// `{symbol}` is replaced with the escaped captured accessor symbol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accessor_symbol_read_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigKeyUsagePattern {
    pub pattern: String,
    #[serde(default = "default_config_key_capture")]
    pub key_capture: String,
    /// Optional symbol capture for accessor definitions. If present, core also
    /// treats non-test references to that symbol outside the definition file as reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_capture: Option<String>,
}

fn default_config_key_capture() -> String {
    "key".to_string()
}
