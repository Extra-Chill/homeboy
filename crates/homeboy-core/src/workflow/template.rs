//! Workflow templating: string substitution only.
//!
//! `{{inputs.x}}`, `{{steps.<id>.output<pointer>}}`, `{{steps.<id>.run_id}}`,
//! `{{instance}}`, and inside a `foreach` step `{{item}}` or
//! `{{item.<field>[.<field>...]}}`. A string value substitutes as itself; any
//! other JSON value substitutes as its compact JSON text.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// One parsed `{{ ... }}` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Input(String),
    StepOutput {
        step: String,
        pointer: String,
    },
    StepRunId(String),
    Instance,
    /// The current `foreach` item, at a JSON pointer (`""` for the item).
    Item(String),
}

/// Values a template reads. Implementations come from the instance state.
pub trait TemplateValues {
    fn input(&self, name: &str) -> Option<&Value>;
    fn step_output(&self, step: &str) -> Option<&Value>;
    fn step_run_id(&self, step: &str) -> Option<&str>;
    fn instance(&self) -> &str;
    /// The current `foreach` item, inside a fan-out step only.
    fn item(&self) -> Option<&Value> {
        None
    }
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
        "instance" => return Ok(Reference::Instance),
        "item" => return Ok(Reference::Item(String::new())),
        _ => {}
    }
    if let Some(path) = raw.strip_prefix("item.") {
        let fields = path.split('.').collect::<Vec<_>>();
        if fields.iter().all(|field| valid_name(field)) {
            return Ok(Reference::Item(
                fields.iter().map(|field| format!("/{field}")).collect(),
            ));
        }
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

/// Resolve `text` to a JSON value: a text that is exactly one reference
/// yields the referenced value itself; any other text renders to a string.
pub fn resolve_value(text: &str, values: &dyn TemplateValues) -> Result<Value, String> {
    let trimmed = text.trim();
    if let Some(capture) = pattern().captures(trimmed) {
        if capture.get(0).expect("whole match").as_str() == trimmed {
            let reference = parse_reference(&capture[1])?;
            return match &reference {
                Reference::Instance | Reference::StepRunId(_) => {
                    resolve(&reference, values).map(Value::String)
                }
                Reference::Input(name) => values
                    .input(name)
                    .cloned()
                    .ok_or_else(|| format!("input `{name}` has no value")),
                Reference::StepOutput { step, pointer } => values
                    .step_output(step)
                    .and_then(|output| output.pointer(pointer))
                    .cloned()
                    .ok_or_else(|| format!("step `{step}` output has no value at `{pointer}`")),
                Reference::Item(pointer) => values
                    .item()
                    .and_then(|item| item.pointer(pointer))
                    .cloned()
                    .ok_or_else(|| format!("item has no value at `{pointer}`")),
            };
        }
    }
    render(text, values).map(Value::String)
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
        Reference::Item(pointer) => values
            .item()
            .and_then(|item| item.pointer(pointer))
            .ok_or_else(|| format!("item has no value at `{pointer}`"))?,
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
        fn instance(&self) -> &str {
            "wf-x-abc"
        }
        fn item(&self) -> Option<&Value> {
            Some(&self.output)
        }
    }

    #[test]
    fn items_render_and_whole_references_resolve_to_values() {
        let values = Values {
            input: json!({"issues": ["a", "b"]}),
            output: json!({"report_path": "/tmp/r.json", "list": [1, 2]}),
        };
        assert_eq!(
            render("{{item.report_path}} {{item.list}}", &values).unwrap(),
            "/tmp/r.json [1,2]"
        );
        assert_eq!(
            resolve_value(" {{inputs.issues}} ", &values).unwrap(),
            json!(["a", "b"])
        );
        assert_eq!(
            resolve_value("{{steps.evaluate.output/list}}", &values).unwrap(),
            json!([1, 2])
        );
        assert_eq!(
            resolve_value("x {{inputs.issues}}", &values).unwrap(),
            json!("x [\"a\",\"b\"]")
        );
        assert!(resolve_value("{{item.missing}}", &values).is_err());
    }

    #[test]
    fn renders_every_reference_kind() {
        let values = Values {
            input: json!({"url": "https://example.test", "n": 3}),
            output: json!({"report_path": "/tmp/r.json", "list": [1, 2]}),
        };
        assert_eq!(
            render(
                "fix {{steps.evaluate.output/report_path}} for {{ inputs.url }} n={{inputs.n}} in {{instance}} run={{steps.fix.run_id}} all={{steps.evaluate.output/list}}",
                &values
            )
            .unwrap(),
            "fix /tmp/r.json for https://example.test n=3 in wf-x-abc run=cook-1 all=[1,2]"
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
        assert_eq!(
            references("{{item}} {{item.issue_url}} {{item.a.b}}").unwrap(),
            vec![
                Reference::Item(String::new()),
                Reference::Item("/issue_url".into()),
                Reference::Item("/a/b".into()),
            ]
        );
        for bad in [
            "{{item.}}",
            "{{item.a..b}}",
            "{{env.HOME}}",
            "{{iteration}}",
            "{{steps.a.status}}",
            "{{steps.a.outputx}}",
            "{{inputs.}}",
        ] {
            assert!(references(bad).is_err(), "{bad}");
        }
    }
}
