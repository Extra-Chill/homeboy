pub const INDEX: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/index.md"));
pub const PROJECTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/projects.md"));
pub const PROJECT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/project.md"));
pub const PROJECT_SUBCOMMANDS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/project-subcommands.md"));

pub fn resolve(topic: &[String]) -> (&'static str, &'static str) {
    if topic.is_empty() {
        return ("index", INDEX);
    }

    let normalized: Vec<String> = topic
        .iter()
        .map(|t| t.to_lowercase())
        .collect();

    if normalized == ["projects".to_string()] {
        return ("projects", PROJECTS);
    }

    if normalized == ["project".to_string()] {
        return ("project", PROJECT);
    }

    if normalized.len() >= 2 && normalized[0] == "project" {
        return ("project subcommands", PROJECT_SUBCOMMANDS);
    }

    ("unknown", "")
}

pub fn available_topics() -> &'static str {
    "index, projects, project, project subcommands"
}
