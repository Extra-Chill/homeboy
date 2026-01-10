pub const INDEX: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/index.md"));

pub const PROJECTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/projects.md"));
pub const PROJECT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/project.md"));
pub const PROJECT_SUBCOMMANDS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/project-subcommands.md"));

pub const SERVER: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/server.md"));
pub const SSH: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/ssh.md"));
pub const WP: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/wp.md"));
pub const PM2: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/pm2.md"));
pub const DB: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/db.md"));
pub const DEPLOY: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/deploy.md"));
pub const COMPONENT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/component.md"));
pub const FILE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/file.md"));
pub const LOGS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/logs.md"));
pub const PIN: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/pin.md"));
pub const MODULE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/module.md"));
pub const GIT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/git.md"));

pub fn resolve(topic: &[String]) -> (&'static str, &'static str) {
    if topic.is_empty() {
        return ("index", INDEX);
    }

    let normalized: Vec<String> = topic
        .iter()
        .map(|t| t.to_lowercase())
        .collect();

    if normalized.len() == 1 {
        if let Some(key) = normalized.first() {
            return match key.as_str() {
                "projects" => ("projects", PROJECTS),
                "project" => ("project", PROJECT),
                "server" => ("server", SERVER),
                "ssh" => ("ssh", SSH),
                "wp" => ("wp", WP),
                "pm2" => ("pm2", PM2),
                "db" => ("db", DB),
                "deploy" => ("deploy", DEPLOY),
                "component" => ("component", COMPONENT),
                "file" => ("file", FILE),
                "logs" => ("logs", LOGS),
                "pin" => ("pin", PIN),
                "module" => ("module", MODULE),
                "git" => ("git", GIT),
                _ => ("unknown", ""),
            };
        }
    }

    if normalized.len() >= 2 && normalized[0] == "project" {
        return ("project subcommands", PROJECT_SUBCOMMANDS);
    }

    ("unknown", "")
}

pub fn available_topics() -> &'static str {
    "index, projects, project, project subcommands, server, ssh, wp, pm2, db, deploy, component, file, logs, pin, module, git"
}
