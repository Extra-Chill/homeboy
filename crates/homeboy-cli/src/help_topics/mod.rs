use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use homeboy::core::engine::text;
use homeboy_core::extension::catalog::load_all_extensions;

include!(concat!(env!("OUT_DIR"), "/generated_docs.rs"));

thread_local! {
    static CURRENT_COMMAND: RefCell<Option<clap::Command>> = const { RefCell::new(None) };
}

fn docs_index() -> &'static HashMap<&'static str, &'static str> {
    static DOCS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

    DOCS.get_or_init(|| GENERATED_DOCS.iter().copied().collect())
}

#[derive(Debug, Clone)]
pub struct ResolvedDoc {
    pub content: String,
}

pub fn resolve(topic: &[String]) -> homeboy::core::Result<ResolvedDoc> {
    let (_, key, _) = normalize_topic(topic);

    if key == "commands/commands-index" {
        return Ok(ResolvedDoc {
            content: crate::cli_surface::reference_docs::generated_command_index(&current_command()),
        });
    }
    if let Some(name) = key.strip_prefix("reference/cli/commands/") {
        if let Some(content) = crate::cli_surface::reference_docs::generated_command_reference(
            &current_command(),
            name,
        ) {
            return Ok(ResolvedDoc { content });
        }
    }

    // Try exact match first (existing behavior)
    if let Some(content) = docs_index().get(key.as_str()).copied() {
        return Ok(ResolvedDoc {
            content: content.to_string(),
        });
    }

    // Then check extension docs (existing behavior)
    if let Some((content, _extension_id)) = load_extension_doc(&key) {
        return Ok(ResolvedDoc { content });
    }

    // Try fallback prefixes for common shortcuts
    let fallback_keys = vec![
        format!("commands/{}", key),
        format!("documentation/{}", key),
        format!("{}/{}-index", key, key),
    ];

    for fallback_key in fallback_keys {
        if let Some(content) = docs_index().get(fallback_key.as_str()).copied() {
            return Ok(ResolvedDoc {
                content: content.to_string(),
            });
        }

        if let Some((content, _extension_id)) = load_extension_doc(&fallback_key) {
            return Ok(ResolvedDoc { content });
        }
    }

    Err(homeboy::core::Error::docs_topic_not_found(&key))
}

fn load_extension_doc(topic: &str) -> Option<(String, String)> {
    for extension in load_all_extensions().unwrap_or_default() {
        let Some(extension_path) = &extension.extension_path else {
            continue;
        };
        let doc_file = Path::new(extension_path)
            .join("docs")
            .join(format!("{}.md", topic));
        if let Ok(content) = std::fs::read_to_string(&doc_file) {
            return Some((content, extension.id));
        }
    }
    None
}

fn normalize_topic(topic: &[String]) -> (String, String, Vec<String>) {
    if topic.is_empty() {
        return (
            "index".to_string(),
            "index".to_string(),
            vec!["index".to_string()],
        );
    }

    let user_label = topic.join(" ");

    let mut segments: Vec<String> = Vec::new();
    for raw in topic {
        for part in raw.split('/') {
            let segment = text::normalize_doc_segment(part);
            if !segment.is_empty() {
                segments.push(segment);
            }
        }
    }

    if segments.is_empty() {
        return (
            "unknown".to_string(),
            "index".to_string(),
            vec!["index".to_string()],
        );
    }

    let key = segments.join("/");

    if user_label.is_empty() {
        return ("unknown".to_string(), key, segments);
    }

    (user_label, key, segments)
}

pub(crate) fn available_topics() -> Vec<String> {
    let mut topics: BTreeSet<String> = GENERATED_DOCS
        .iter()
        .map(|(key, _)| key.to_string())
        .collect();
    topics.insert("commands/commands-index".to_string());
    topics.extend(
        crate::cli_surface::reference_docs::documented_subcommands(&current_command())
            .into_iter()
            .map(|command| format!("reference/cli/commands/{}", command.get_name())),
    );

    // Add extension docs (integrated namespace)
    for extension in load_all_extensions().unwrap_or_default() {
        if let Some(extension_path) = &extension.extension_path {
            let docs_dir = Path::new(extension_path).join("docs");
            if docs_dir.exists() {
                collect_doc_topics(&docs_dir, "", &mut topics);
            }
        }
    }

    topics.into_iter().collect()
}

pub(crate) fn with_command<T>(command: Option<clap::Command>, run: impl FnOnce() -> T) -> T {
    struct Reset(Option<clap::Command>);

    impl Drop for Reset {
        fn drop(&mut self) {
            CURRENT_COMMAND.with(|current| current.replace(self.0.take()));
        }
    }

    let previous = CURRENT_COMMAND.with(|current| current.replace(command));
    let _reset = Reset(previous);
    run()
}

fn current_command() -> clap::Command {
    CURRENT_COMMAND
        .with(|current| current.borrow().clone())
        .unwrap_or_else(crate::cli_surface::Cli::command_with_scoped_lab_args)
}

fn collect_doc_topics(dir: &Path, prefix: &str, topics: &mut BTreeSet<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let Some(name) = path.file_name() else {
                    continue;
                };
                let name = name.to_string_lossy();
                let new_prefix = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", prefix, name)
                };
                collect_doc_topics(&path, &new_prefix, topics);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                let Some(stem) = path.file_stem() else {
                    continue;
                };
                let stem = stem.to_string_lossy();
                let topic = if prefix.is_empty() {
                    stem.to_string()
                } else {
                    format!("{}/{}", prefix, stem)
                };
                topics.insert(topic);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// list/resolve contract: every embedded topic printed by `self docs list`
    /// MUST be openable via `self docs <that exact string>`. Guards issue #7607,
    /// where uppercase root doc keys (CHANGELOG, TESTING, ...) were listed but
    /// unresolvable because the resolver lowercases input before lookup.
    #[test]
    fn every_generated_topic_resolves() {
        for (key, _) in GENERATED_DOCS {
            let resolved = resolve(&[key.to_string()]);
            assert!(
                resolved.is_ok(),
                "topic '{key}' is listed by GENERATED_DOCS but does not resolve"
            );
        }
    }

    /// Generated keys must already be in normalized (resolver-facing) form, so
    /// list output and resolver lookups share one casing. A drift here is the
    /// exact defect issue #7607 documents.
    #[test]
    fn generated_keys_are_normalized() {
        for (key, _) in GENERATED_DOCS {
            let normalized = key
                .split('/')
                .map(text::normalize_doc_segment)
                .collect::<Vec<_>>()
                .join("/");
            assert_eq!(
                *key, normalized,
                "generated doc key '{key}' is not in normalized form (expected '{normalized}')"
            );
        }
    }

    #[test]
    fn command_index_is_derived_from_the_current_cli() {
        let index = resolve(&["commands/commands-index".to_string()]).expect("command index");

        assert!(index.content.contains("# Commands index"));
        assert!(index.content.contains("- [agent-task](agent-task.md)"));
        assert!(available_topics().contains(&"commands/commands-index".to_string()));
    }

    #[test]
    fn generated_command_reference_topics_remain_resolvable() {
        let topic = "reference/cli/commands/agent-task".to_string();
        let reference = resolve(std::slice::from_ref(&topic)).expect("generated reference");

        assert!(reference.content.contains("Run generic agent task plans"));
        assert!(reference.content.contains("Usage: homeboy agent-task"));
        assert!(available_topics().contains(&topic));
    }
}
