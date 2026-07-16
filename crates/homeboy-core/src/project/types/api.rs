use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub header: String,
    #[serde(default)]
    pub variables: HashMap<String, VariableSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<AuthFlowConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<AuthFlowConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableSource {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthFlowConfig {
    pub endpoint: String,
    #[serde(default = "default_post_method")]
    pub method: String,
    #[serde(default)]
    pub body: HashMap<String, String>,
    #[serde(default)]
    pub store: HashMap<String, String>,
}

fn default_post_method() -> String {
    "POST".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTarget {
    pub name: String,
    pub domain: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i32>,
    #[serde(default)]
    pub is_default: bool,
}

impl SubTarget {
    pub fn table_prefix(&self, base_prefix: &str) -> String {
        match self.number {
            Some(n) if n > 1 => format!("{}{}_", base_prefix, n),
            _ => base_prefix.to_string(),
        }
    }
}
