use crate::module::{load_module, ModuleManifest};
use crate::template::{render, TemplateVars};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildCommandSource {
    Module,
}

pub struct BuildCommandCandidate {
    pub source: BuildCommandSource,
    pub command: String,
}

fn file_exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// Detect build command using module configuration.
pub fn detect_build_command(
    local_path: &str,
    build_artifact: &str,
    modules: &[String],
) -> Option<BuildCommandCandidate> {
    let root = PathBuf::from(local_path);

    // Check modules for matching build config
    for module_id in modules {
        if let Some(module) = load_module(module_id) {
            if let Some(candidate) = detect_build_from_module(&root, build_artifact, &module) {
                return Some(candidate);
            }
        }
    }

    None
}

fn detect_build_from_module(
    root: &Path,
    build_artifact: &str,
    module: &ModuleManifest,
) -> Option<BuildCommandCandidate> {
    let build_config = module.build.as_ref()?;

    // Check if artifact matches any configured extension
    let artifact_lower = build_artifact.to_ascii_lowercase();
    let matches_artifact = build_config
        .artifact_extensions
        .iter()
        .any(|ext| artifact_lower.ends_with(&ext.to_ascii_lowercase()));

    if !matches_artifact {
        return None;
    }

    // Look for any of the configured script names
    for script_name in &build_config.script_names {
        let script_path = root.join(script_name);
        if file_exists(&script_path) {
            let command = build_config
                .command_template
                .as_ref()
                .map(|tpl| render(tpl, &[(TemplateVars::SCRIPT, script_name)]))
                .unwrap_or_else(|| format!("sh {}", script_name));

            return Some(BuildCommandCandidate {
                source: BuildCommandSource::Module,
                command,
            });
        }
    }

    None
}

pub fn detect_zip_single_root_dir(zip_path: &Path) -> crate::Result<Option<String>> {
    let file = std::fs::File::open(zip_path)
        .map_err(|err| crate::Error::internal_io(err.to_string(), Some("open zip".to_string())))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|err| crate::Error::internal_unexpected(format!("Zip parse error: {}", err)))?;

    let mut roots: BTreeSet<String> = BTreeSet::new();

    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|err| {
            crate::Error::internal_unexpected(format!("Zip entry error: {}", err))
        })?;
        let name = entry.name();

        let mut parts = name.split('/').filter(|p| !p.is_empty());
        let Some(first) = parts.next() else {
            continue;
        };

        if first == "__MACOSX" || first == ".DS_Store" {
            continue;
        }

        roots.insert(first.to_string());

        if roots.len() > 1 {
            return Ok(None);
        }
    }

    Ok(roots.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn no_detection_without_modules() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::write(temp_dir.path().join("build.sh"), "#!/bin/sh\necho ok\n").unwrap();

        // Without modules, no build command is detected (module-driven detection)
        let candidate =
            detect_build_command(temp_dir.path().to_str().unwrap(), "dist/plugin.zip", &[]);
        assert!(candidate.is_none());
    }

    #[test]
    fn detects_single_root_dir_in_zip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let zip_path = temp_dir.path().join("plugin.zip");

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::default();

            zip.add_directory("sell-my-images/", options).unwrap();
            zip.start_file("sell-my-images/sell-my-images.php", options)
                .unwrap();
            zip.write_all(b"<?php\n/*\nPlugin Name: Sell My Images\n*/\n")
                .unwrap();
            zip.finish().unwrap();
        }

        let root = detect_zip_single_root_dir(&zip_path).unwrap();
        assert_eq!(root.as_deref(), Some("sell-my-images"));
    }

    #[test]
    fn returns_none_for_multiple_root_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let zip_path = temp_dir.path().join("mixed.zip");

        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::default();

            zip.start_file("one/a.txt", options).unwrap();
            zip.write_all(b"a").unwrap();
            zip.start_file("two/b.txt", options).unwrap();
            zip.write_all(b"b").unwrap();
            zip.finish().unwrap();
        }

        let root = detect_zip_single_root_dir(&zip_path).unwrap();
        assert!(root.is_none());
    }
}
