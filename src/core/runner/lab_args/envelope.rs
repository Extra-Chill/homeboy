//! Typed Lab execution input envelope.
//!
//! This is intentionally small for the first migration step: it captures the
//! argv plus the Lab-relevant input refs discovered before `--`. Existing
//! rewrite helpers can consume the typed refs one path at a time while preserving
//! the exact argv output they already produced.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core::runner) struct ExecutionEnvelope {
    argv: Vec<String>,
    pub inputs: LabCommandInputs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::core::runner) struct LabCommandInputs {
    pub provider_configs: Vec<ArgRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core::runner) struct ArgRef {
    pub flag: &'static str,
    pub value: ArgValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core::runner) enum ArgValue {
    InlineText(String),
    PathRef(String),
    Stdin,
    Missing,
}

impl ExecutionEnvelope {
    pub fn from_args(args: &[String]) -> Self {
        let mut inputs = LabCommandInputs::default();
        let mut iter = args.iter().peekable();
        let mut passthrough = false;
        while let Some(arg) = iter.next() {
            if passthrough {
                continue;
            }
            if arg == "--" {
                passthrough = true;
                continue;
            }
            if arg == "--provider-config" {
                inputs.provider_configs.push(ArgRef {
                    flag: "--provider-config",
                    value: iter
                        .next()
                        .map_or(ArgValue::Missing, |spec| provider_config_value(spec)),
                });
                continue;
            }
            if let Some(spec) = arg.strip_prefix("--provider-config=") {
                inputs.provider_configs.push(ArgRef {
                    flag: "--provider-config",
                    value: provider_config_value(spec),
                });
            }
        }

        Self {
            argv: args.to_vec(),
            inputs,
        }
    }

    pub fn rewrite_provider_config_values(
        &self,
        mut rewrite: impl FnMut(&str) -> String,
    ) -> Vec<String> {
        let mut out = Vec::with_capacity(self.argv.len());
        let mut iter = self.argv.iter().peekable();
        let mut passthrough = false;
        while let Some(arg) = iter.next() {
            if passthrough {
                out.push(arg.clone());
                continue;
            }
            if arg == "--" {
                passthrough = true;
                out.push(arg.clone());
                continue;
            }
            if arg == "--provider-config" {
                out.push(arg.clone());
                if let Some(spec) = iter.next() {
                    out.push(rewrite(spec));
                }
                continue;
            }
            if let Some(spec) = arg.strip_prefix("--provider-config=") {
                out.push(format!("--provider-config={}", rewrite(spec)));
                continue;
            }
            out.push(arg.clone());
        }
        out
    }
}

fn provider_config_value(spec: &str) -> ArgValue {
    let trimmed = spec.trim();
    if trimmed == "-" {
        ArgValue::Stdin
    } else if let Some(path) = trimmed.strip_prefix('@') {
        ArgValue::PathRef(path.to_string())
    } else {
        ArgValue::InlineText(spec.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_captures_provider_config_refs_before_passthrough() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--provider-config".to_string(),
            "@config.json".to_string(),
            "--provider-config={\"provider\":\"codex\"}".to_string(),
            "--provider-config=-".to_string(),
            "--".to_string(),
            "--provider-config".to_string(),
            "ignored".to_string(),
        ];

        let envelope = ExecutionEnvelope::from_args(&args);

        assert_eq!(envelope.inputs.provider_configs.len(), 3);
        assert_eq!(
            envelope.inputs.provider_configs[0].flag,
            "--provider-config"
        );
        assert_eq!(
            envelope.inputs.provider_configs[0].value,
            ArgValue::PathRef("config.json".to_string())
        );
        assert_eq!(
            envelope.inputs.provider_configs[1].value,
            ArgValue::InlineText("{\"provider\":\"codex\"}".to_string())
        );
        assert_eq!(envelope.inputs.provider_configs[2].value, ArgValue::Stdin);
    }

    #[test]
    fn envelope_captures_missing_provider_config_value() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--provider-config".to_string(),
        ];

        let envelope = ExecutionEnvelope::from_args(&args);

        assert_eq!(envelope.inputs.provider_configs.len(), 1);
        assert_eq!(envelope.inputs.provider_configs[0].value, ArgValue::Missing);
    }
}
