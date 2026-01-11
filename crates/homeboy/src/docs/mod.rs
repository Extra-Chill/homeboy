use std::fs;
use std::path::{Path, PathBuf};

use homeboy_core::token;

fn docs_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs")
}

pub fn resolve(topic: &[String]) -> (String, String) {
    let doc_path = topic_to_doc_path(topic);
    let label = topic.join(" ");

    let content = fs::read_to_string(&doc_path).unwrap_or_default();

    let topic_label = if topic.is_empty() {
        "index".to_string()
    } else if label.is_empty() {
        "unknown".to_string()
    } else {
        label
    };

    (topic_label, content)
}

fn topic_to_doc_path(topic: &[String]) -> PathBuf {
    if topic.is_empty() {
        return docs_root().join("index.md");
    }

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
        return docs_root().join("index.md");
    }

    let mut path = docs_root();
    for segment in segments {
        path = path.join(segment);
    }

    path.set_extension("md");
    path
}

pub fn available_topics() -> &'static str {
    "Use path tokens like: `homeboy docs`, `homeboy docs cli server`, `homeboy docs cli/server`, `homeboy docs config app-config`"
}
