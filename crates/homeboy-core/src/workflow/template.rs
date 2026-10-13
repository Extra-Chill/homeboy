//! Workflow templating: string substitution only.
//!
//! `{{inputs.x}}`, `{{steps.<id>.output<pointer>}}`, `{{steps.<id>.run_id}}`,
//! `{{iteration}}` and `{{instance}}`. A string value substitutes as itself;
//! any other JSON value substitutes as its compact JSON text.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// One parsed `{{ ... }}` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Input(String),
    StepOutput { step: String, pointer: String },
    StepRunId(String),
    Iteration,
    Instance,
}

/// Values a template reads. Implementations come from the instance state.
pub trait TemplateValues {
    fn input(&self, name: &str) -> Option<&Value>;
    fn step_output(&self, step: &str) -> Option<&Value>;
    fn step_run_id(&self, step: &str) -> Option<&str>;
    fn iteration(&self) -> u32;
    fn instance(&self) -> &str;
}

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\{\{\s*([^{}]*?)\s*\}\}").expect("template pattern"))
}

/// Every reference in `text`, or the first malformed one.
pub fn references(text: &str) -> Result<Vec<Reference>, String> {
    pattern()
        .captures_iter(text)
        .map(|capture| parse_reference(&capture[1]))
        .collect()
}

fn parse_reference(raw: &str) -> Result<Reference, String> {
    match raw {
        "iteration" => return Ok(Reference::Iteration),
        "instance" => return Ok(Reference::Instance),
        "item" => return Err(
            "`{{item}}` belongs to `foreach` fan-out, which this workflow version does not support"
                .to_string(),
        ),
        _ => {}
    }
    if let Some(name) = raw.strip_prefix("inputs.") {
        if valid_name(name) {
            return Ok(Reference::Input(name.to_string()));
        }
    }
    if let Some(rest) = raw.strip_prefix("steps.") {
        if let Some((step, field)) = rest.split_once('.') {
            if valid_name(step) {
                if field == "run_id" {
                    return Ok(Reference::StepRunId(step.to_string()));
                }
                if let Some(pointer) = field.strip_prefix("output") {
                    if pointer.is_empty() || pointer.starts_with('/') {
                        return Ok(Reference::StepOutput {
                            step: step.to_string(),
                            pointer: pointer.to_string(),
                        });
                    }
                }
            }
        }
    }
    Err(format!("unknown template reference `{{{{{raw}}}}}`"))
}

/// Names of inputs and steps: letters, digits, `_` and `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// Substitute every reference in `text`.
pub fn render(text: &str, values: &dyn TemplateValues) -> Result<String, String> {
    let mut rendered = String::with_capacity(text.len());
    let mut last = 0;
    for capture in pattern().captures_iter(text) {
        let whole = capture.get(0).expect("whole match");
        rendered.push_str(&text[last..whole.start()]);
        rendered.push_str(&resolve(&parse_reference(&capture[1])?, values)?);
        last = whole.end();
    }
    rendered.push_str(&text[last..]);
    Ok(rendered)
}

fn resolve(reference: &Reference, values: &dyn TemplateValues) -> Result<String, String> {
    let value = match reference {
        Reference::Iteration => return Ok(values.iteration().to_string()),
        Reference::Instance => return Ok(values.instance().to_string()),
        Reference::Input(name) => values
            .input(name)
            .ok_or_else(|| format!("input `{name}` has no value"))?,
        Reference::StepRunId(step) => {
            return values
                .step_run_id(step)
                .map(str::to_string)
                .ok_or_else(|| format!("step `{step}` has no run id"))
        }
        Reference::StepOutput { step, pointer } => values
            .step_output(step)
            .and_then(|output| output.pointer(pointer))
            .ok_or_else(|| format!("step `{step}` output has no value at `{pointer}`"))?,
    };
    Ok(match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Values {
        input: Value,
        output: Value,
    }

    impl TemplateValues for Values {
        fn input(&self, name: &str) -> Option<&Value> {
            self.input.get(name)
        }
        fn step_output(&self, step: &str) -> Option<&Value> {
            (step == "evaluate").then_some(&self.output)
        }
        fn step_run_id(&self, step: &str) -> Option<&str> {
            (step == "fix").then_some("cook-1")
        }
        fn iteration(&self) -> u32 {
            2
        }
        fn instance(&self) -> &str {
            "wf-x-abc"
        }
    }

    #[test]
    fn renders_every_reference_kind() {
        let values = Values {
            input: json!({"url": "https://example.test", "n": 3}),
            output: json!({"report_path": "/tmp/r.json", "list": [1, 2]}),
        };
        assert_eq!(
            render(
                "fix {{steps.evaluate.output/report_path}} for {{ inputs.url }} n={{inputs.n}} it={{iteration}} in {{instance}} run={{steps.fix.run_id}} all={{steps.evaluate.output/list}}",
                &values
            )
            .unwrap(),
            "fix /tmp/r.json for https://example.test n=3 it=2 in wf-x-abc run=cook-1 all=[1,2]"
        );
        assert!(render("{{steps.evaluate.output/missing}}", &values).is_err());
        assert!(render("{{inputs.absent}}", &values).is_err());
    }

    #[test]
    fn references_are_a_closed_set() {
        assert_eq!(
            references("{{steps.a.output}} {{steps.b.run_id}}").unwrap(),
            vec![
                Reference::StepOutput {
                    step: "a".into(),
                    pointer: String::new()
                },
                Reference::StepRunId("b".into())
            ]
        );
        for bad in [
            "{{item}}",
            "{{env.HOME}}",
            "{{steps.a.status}}",
            "{{steps.a.outputx}}",
            "{{inputs.}}",
        ] {
            assert!(references(bad).is_err(), "{bad}");
        }
    }
}
