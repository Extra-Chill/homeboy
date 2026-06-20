//! Typed Lab execution input envelope.
//!
//! This is intentionally small for the first migration step: it captures the
//! argv plus the Lab-relevant input refs discovered before `--`. Existing
//! rewrite helpers can consume the typed refs one path at a time while preserving
//! the exact argv output they already produced.

use super::path_remap::{rewrite_flag_value_args, try_rewrite_flag_value_args};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::core::runner) struct ExecutionEnvelope {
    argv: Vec<String>,
    pub inputs: LabCommandInputs,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::core::runner) struct LabCommandInputs {
    pub provider_configs: Vec<ArgRef>,
    pub agent_task_text_specs: Vec<ArgRef>,
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
                continue;
            }
            if let Some(flag) = agent_task_text_flag(arg) {
                inputs.agent_task_text_specs.push(ArgRef {
                    flag,
                    value: iter
                        .next()
                        .map_or(ArgValue::Missing, |spec| agent_task_text_value(spec)),
                });
                continue;
            }
            if let Some((flag, spec)) = agent_task_text_inline_arg(arg) {
                inputs.agent_task_text_specs.push(ArgRef {
                    flag,
                    value: agent_task_text_value(spec),
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
        rewrite_flag_value_args(&self.argv, |arg, iter, out| {
            if arg == "--provider-config" {
                out.push(arg.to_string());
                if let Some(spec) = iter.next() {
                    out.push(rewrite(spec));
                }
                return;
            }
            if let Some(spec) = arg.strip_prefix("--provider-config=") {
                out.push(format!("--provider-config={}", rewrite(spec)));
                return;
            }
            out.push(arg.to_string());
        })
    }

    pub fn rewrite_agent_task_text_values(
        &self,
        mut rewrite: impl FnMut(&str, &str) -> crate::core::Result<String>,
    ) -> crate::core::Result<Vec<String>> {
        try_rewrite_flag_value_args(&self.argv, |arg, iter, out| {
            if let Some(flag) = agent_task_text_flag(arg) {
                out.push(arg.to_string());
                if let Some(spec) = iter.next() {
                    out.push(rewrite(spec, flag)?);
                }
                return Ok(());
            }
            if let Some((flag, spec)) = agent_task_text_inline_arg(arg) {
                out.push(format!("{}={}", flag, rewrite(spec, flag)?));
                return Ok(());
            }
            out.push(arg.to_string());
            Ok(())
        })
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

fn agent_task_text_value(spec: &str) -> ArgValue {
    let trimmed = spec.trim();
    if trimmed == "-" {
        ArgValue::Stdin
    } else if let Some(path) = trimmed.strip_prefix('@') {
        ArgValue::PathRef(path.to_string())
    } else {
        ArgValue::InlineText(spec.to_string())
    }
}

fn agent_task_text_flag(arg: &str) -> Option<&'static str> {
    match arg {
        "--prompt" => Some("--prompt"),
        "--task" => Some("--task"),
        "--tasks" => Some("--tasks"),
        _ => None,
    }
}

fn agent_task_text_inline_arg(arg: &str) -> Option<(&'static str, &str)> {
    for flag in ["--prompt", "--task", "--tasks"] {
        if let Some(value) = arg
            .strip_prefix(flag)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some((flag, value));
        }
    }
    None
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

    #[test]
    fn envelope_captures_agent_task_text_refs_before_passthrough() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "@prompt.md".to_string(),
            "--task=Fix it".to_string(),
            "--tasks=-".to_string(),
            "--".to_string(),
            "--prompt".to_string(),
            "@ignored.md".to_string(),
        ];

        let envelope = ExecutionEnvelope::from_args(&args);

        assert_eq!(envelope.inputs.agent_task_text_specs.len(), 3);
        assert_eq!(envelope.inputs.agent_task_text_specs[0].flag, "--prompt");
        assert_eq!(
            envelope.inputs.agent_task_text_specs[0].value,
            ArgValue::PathRef("prompt.md".to_string())
        );
        assert_eq!(envelope.inputs.agent_task_text_specs[1].flag, "--task");
        assert_eq!(
            envelope.inputs.agent_task_text_specs[1].value,
            ArgValue::InlineText("Fix it".to_string())
        );
        assert_eq!(envelope.inputs.agent_task_text_specs[2].flag, "--tasks");
        assert_eq!(
            envelope.inputs.agent_task_text_specs[2].value,
            ArgValue::Stdin
        );
    }
}
