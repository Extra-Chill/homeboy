//! Shared content-addressed package artifacts. Failure to identify inputs disables reuse.
use super::*;
use sha2::{Digest, Sha256};
use std::{fs, process::Command};

fn output(path: &Path, program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let result = Command::new(program)
        .args(args)
        .current_dir(path)
        .output()
        .ok()?;
    result.status.success().then_some(result.stdout)
}

pub fn validate_graph(component: &Component) -> Result<()> {
    fn visit(component: &Component, trail: &mut Vec<PathBuf>) -> Result<()> {
        let path = paths::canonical_or_normalized_local_path(&component.local_path);
        if trail.contains(&path) {
            return Err(Error::validation_invalid_argument(
                "build.dependencies",
                format!("Workspace dependency cycle at {}", path.display()),
                None,
                None,
            ));
        }
        trail.push(path);
        for dependency in dependencies(component)? {
            visit(&dependency, trail)?;
        }
        trail.pop();
        Ok(())
    }
    visit(component, &mut Vec::new())
}

pub fn content_key(component: &Component) -> Option<String> {
    key(component, &mut Vec::new())
}

fn key(component: &Component, trail: &mut Vec<PathBuf>) -> Option<String> {
    let root = fs::canonicalize(&component.local_path).ok()?;
    if trail.contains(&root) || !component.artifact_inputs.is_empty() || !component.env.is_empty() {
        return None;
    }
    let dirty = output(
        &root,
        "git",
        &["status", "--porcelain", "--untracked-files=all", "--", "."],
    )?;
    if !dirty.is_empty() {
        return None;
    }
    let index = output(&root, "git", &["ls-files", "-s", "-z", "--", "."])?;
    if index.is_empty() {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"homeboy-package-v1\0");
    hash.update(index);
    // Settings, component scripts and artifact configuration are inputs too.
    let mut config = serde_json::to_value(component).ok()?;
    config.as_object_mut()?.remove("local_path");
    hash.update(serde_json::to_vec(&config).ok()?);
    let resolved = resolve_build_command(component).ok()?;
    trail.push(root.clone());
    let mut ids: Vec<_> = component.extensions.as_ref()?.keys().collect();
    ids.sort();
    let mut declared = false;
    for id in ids {
        let extension = extension::catalog::load_extension(id).ok()?;
        let Some(build) = extension.build.as_ref() else {
            continue;
        };
        let inputs = build.cache.as_ref()?;
        declared = true;
        hash.update(id.as_bytes());
        hash.update(extension.version.as_bytes());
        hash.update(inputs.recipe.as_bytes());
        hash.update(serde_json::to_vec(build).ok()?);
        if let Some(script) = &build.extension_script {
            hash.update(fs::read(paths::extension(id).ok()?.join(script)).ok()?);
        }
        if let Some(script) = &build.pre_build_script {
            hash.update(fs::read(paths::extension(id).ok()?.join(script)).ok()?);
        }
        for command in &inputs.toolchains {
            hash.update(command.as_bytes());
            hash.update(output(&root, "sh", &["-c", command])?);
        }
    }
    for dependency in dependencies(component).ok()? {
        hash.update(key(&dependency, trail)?.as_bytes());
    }
    trail.pop();
    // Commands with absolute extension paths must not tie keys to checkout location.
    if let ResolvedBuildCommand::ComponentScript { command } = resolved {
        hash.update(command.as_bytes());
    }
    declared.then(|| format!("{:x}", hash.finalize()))
}

/// Resolve extension-declared workspace input paths, without technology-specific policy.
pub fn dependencies(component: &Component) -> Result<Vec<Component>> {
    let mut result = Vec::new();
    let config = serde_json::to_value(component).unwrap_or_default();
    if let Some(extensions) = &component.extensions {
        let mut ids: Vec<_> = extensions.keys().collect();
        ids.sort();
        for id in ids {
            let extension = extension::catalog::load_extension(id)?;
            let setting = extension
                .build
                .as_ref()
                .and_then(|b| b.cache.as_ref())
                .and_then(|c| c.dependencies_setting.as_ref());
            let Some(setting) = setting else { continue };
            if let Some(entries) = config["extensions"][id][setting].as_array() {
                for entry in entries {
                    let Some(path) = entry["path"].as_str() else {
                        continue;
                    };
                    let root = Path::new(&component.local_path)
                        .join(path)
                        .join(entry["package_dir"].as_str().unwrap_or(""));
                    let dependency =
                        component::try_discover_from_portable(&root)?.ok_or_else(|| {
                            Error::validation_invalid_argument(
                                "build.dependencies",
                                format!("Missing homeboy.json in {}", root.display()),
                                None,
                                None,
                            )
                        })?;
                    if !result
                        .iter()
                        .any(|c: &Component| c.local_path == dependency.local_path)
                    {
                        result.push(dependency);
                    }
                }
            }
        }
    }
    Ok(result)
}

/// A process-wide and cross-process lease prevents duplicate builds for a key.
/// The file handle releases its OS lock even when a build fails or is interrupted.
pub fn lock(key: &str) -> Option<fs::File> {
    let root = directory()?;
    fs::create_dir_all(&root).ok()?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(format!("{key}.lock")))
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

fn directory() -> Option<PathBuf> {
    std::env::var_os("HOMEBOY_BUILD_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| paths::artifact_root().ok().map(|p| p.join("build-cache")))
}

fn artifact(component: &Component) -> Option<PathBuf> {
    let pattern = component::resolve_artifact(component).ok()??;
    resolve_artifact_path_from_root(&pattern, Some(Path::new(&component.local_path))).ok()
}

/// Restore exact package bytes, not dependency install state.
pub fn restore(component: &Component, key: &str) -> bool {
    let Some(dir) = directory().map(|p| p.join(key)) else {
        return false;
    };
    let Ok(name) = fs::read_to_string(dir.join("path")) else {
        return false;
    };
    let relative = Path::new(&name);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let target = Path::new(&component.local_path).join(relative);
    let Some(parent) = target.parent() else {
        return false;
    };
    fs::create_dir_all(parent).is_ok() && fs::copy(dir.join("package"), target).is_ok()
}

pub fn store(component: &Component, key: &str) {
    let Some(root) = directory() else { return };
    let Some(artifact) = artifact(component) else {
        return;
    };
    let Ok(relative) = artifact.strip_prefix(&component.local_path) else {
        return;
    };
    if fs::create_dir_all(&root).is_err() {
        return;
    }
    let Ok(temp) = tempfile::tempdir_in(&root) else {
        return;
    };
    if fs::copy(&artifact, temp.path().join("package")).is_ok()
        && fs::write(
            temp.path().join("path"),
            relative.to_string_lossy().as_bytes(),
        )
        .is_ok()
    {
        let _ = fs::rename(temp.path(), root.join(key));
    }
}
