use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use homeboy::module::load_all_modules;
use homeboy::token;

include!(concat!(env!("OUT_DIR"), "/generated_docs.rs"));

fn docs_index() -> &'static HashMap<&'static str, &'static str> {
    static DOCS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

    DOCS.get_or_init(|| GENERATED_DOCS.iter().copied().collect())
}

#[derive(Debug, Clone)]
pub struct ResolvedDoc {
    pub topic_label: String,
    pub key: String,
    pub segments: Vec<String>,
    pub content: String,
    pub source: String,
}

pub fn resolve(topic: &[String]) -> homeboy::Result<ResolvedDoc> {
    let (topic_label, key, segments) = normalize_topic(topic);

    // First check embedded core docs
    if let Some(content) = docs_index().get(key.as_str()).copied() {
        return Ok(ResolvedDoc {
            topic_label,
            key,
            segments,
            content: content.to_string(),
            source: "core".to_string(),
        });
    }

    // Then check module docs
    if let Some((content, module_id)) = load_module_doc(&key) {
        return Ok(ResolvedDoc {
            topic_label,
            key,
            segments,
            content,
            source: module_id,
        });
    }

    Err(homeboy::Error::config_missing_key(
        format!("docs.{}", key),
        None,
    ))
}

fn load_module_doc(topic: &str) -> Option<(String, String)> {
    for module in load_all_modules() {
        let Some(module_path) = &module.module_path else {
            continue;
        };
        let doc_file = Path::new(module_path)
            .join("docs")
            .join(format!("{}.md", topic));
        if let Ok(content) = std::fs::read_to_string(&doc_file) {
            return Some((content, module.id));
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
            let segment = token::normalize_doc_segment(part);
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

pub fn available_topics() -> Vec<String> {
    let mut topics: BTreeSet<String> = GENERATED_DOCS
        .iter()
        .map(|(key, _)| key.to_string())
        .collect();

    // Add module docs (integrated namespace)
    for module in load_all_modules() {
        if let Some(module_path) = &module.module_path {
            let docs_dir = Path::new(module_path).join("docs");
            if docs_dir.exists() {
                collect_doc_topics(&docs_dir, "", &mut topics);
            }
        }
    }

    topics.into_iter().collect()
}

fn collect_doc_topics(dir: &Path, prefix: &str, topics: &mut BTreeSet<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy();
                let new_prefix = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", prefix, name)
                };
                collect_doc_topics(&path, &new_prefix, topics);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                let stem = path.file_stem().unwrap().to_string_lossy();
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
