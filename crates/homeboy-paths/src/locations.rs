use std::path::PathBuf;

use homeboy_error::Result;

use super::homeboy;

/// Projects directory
pub fn projects() -> Result<PathBuf> {
    Ok(homeboy()?.join("projects"))
}

/// Project directory path (e.g., ~/.config/homeboy/projects/{id}/)
pub fn project_dir(id: &str) -> Result<PathBuf> {
    Ok(projects()?.join(id))
}

/// Project config file path (e.g., ~/.config/homeboy/projects/{id}/{id}.json)
pub fn project_config(id: &str) -> Result<PathBuf> {
    Ok(projects()?.join(id).join(format!("{}.json", id)))
}

/// Servers directory
pub fn servers() -> Result<PathBuf> {
    Ok(homeboy()?.join("servers"))
}

/// Components directory
pub fn components() -> Result<PathBuf> {
    Ok(homeboy()?.join("components"))
}

/// Extensions directory
pub fn extensions() -> Result<PathBuf> {
    Ok(homeboy()?.join("extensions"))
}

/// Agent runtime package directory
pub fn agent_runtimes() -> Result<PathBuf> {
    let root = homeboy()?;
    let generations = root.join("runtime-generations");
    let current = generations.join("current");
    // Generations are opt-in at read time: installs created before #10478 keep
    // their established location until the first successful refresh publishes one.
    Ok(if current_generation(&generations, &current) {
        current.join("agent-runtimes")
    } else {
        root.join("agent-runtimes")
    })
}

fn current_generation(generations: &std::path::Path, current: &std::path::Path) -> bool {
    let Ok(target) = std::fs::read_link(current) else {
        return false;
    };
    if target.is_absolute()
        || target
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return false;
    }
    generations.join(target).join("agent-runtimes").is_dir()
}

/// Legacy runtime package directory, used only while migrating it into a
/// generation. Runtime consumers must use [`agent_runtimes`].
pub fn legacy_agent_runtimes() -> Result<PathBuf> {
    Ok(homeboy()?.join("agent-runtimes"))
}

/// Keys directory
pub fn keys() -> Result<PathBuf> {
    Ok(homeboy()?.join("keys"))
}

/// Backups directory
pub fn backups() -> Result<PathBuf> {
    Ok(homeboy()?.join("backups"))
}
