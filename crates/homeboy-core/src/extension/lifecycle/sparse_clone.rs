//! Prepare URL sources without materializing unrelated monorepo trees.
use std::path::{Component, Path};

use homeboy_core::error::{Error, Result};
use homeboy_core::extension::root_manifest::{ExtensionRootManifest, SharedAssetDeclaration};
use homeboy_core::git;

const ROOT_MANIFEST: &str = "homeboy-extension-root.json";

pub(super) fn clone_extension(
    url: &str,
    target: &Path,
    revision: Option<&str>,
    id: &str,
) -> Result<()> {
    clone_with_runner(url, target, revision, id, |cwd, args| {
        git::run_git_with_env_timeout(
            cwd,
            args,
            "prepare extension source",
            &[],
            super::EXTENSION_SOURCE_PREPARE_TIMEOUT,
        )
    })
}

pub(super) fn clone_with_runner(
    url: &str,
    target: &Path,
    revision: Option<&str>,
    id: &str,
    mut run: impl FnMut(&Path, &[&str]) -> Result<String>,
) -> Result<()> {
    let sparse = (|| {
        let target_path = target.to_string_lossy();
        let mut args = vec![
            "clone",
            "--depth=1",
            "--filter=blob:none",
            "--no-checkout",
            "--sparse",
        ];
        if let Some(revision) = revision {
            args.extend(["--branch", revision]);
        }
        args.extend([url, &target_path]);
        run(Path::new("."), &args)?;
        // --branch selects either a branch tip or a tag as the cloned HEAD.
        let resolved = run(target, &["rev-parse", "--verify", "HEAD"])?;
        let revision = resolved.trim();
        let tree = run(target, &["ls-tree", "-r", "--name-only", "-z", revision])?;
        let files: Vec<_> = tree.split('\0').filter(|s| !s.is_empty()).collect();
        if files.contains(&format!("{id}.json").as_str()) {
            // A root extension owns its whole repository (including runtime files).
            run(target, &["sparse-checkout", "disable"])?;
        } else {
            let mut roots = vec![String::new()];
            let mut nested: Vec<_> = files
                .iter()
                .filter_map(|path| path.strip_suffix(&format!("/{ROOT_MANIFEST}")))
                .filter(|root| !root.split('/').any(|part| part.starts_with('.')))
                .collect();
            nested.sort();
            roots.extend(nested.into_iter().map(|root| format!("{root}/")));
            let root = roots
                .iter()
                .find(|root| files.contains(&format!("{root}{id}/{id}.json").as_str()))
                .ok_or_else(|| Error::internal_io("No sparse extension layout found", None))?;
            let mut paths = vec![format!("{root}{id}")];
            let manifest_path = format!("{root}{ROOT_MANIFEST}");
            if files.contains(&manifest_path.as_str()) {
                let raw = run(target, &["show", &format!("{revision}:{manifest_path}")])?;
                // Match the resolver's treatment of invalid root declarations.
                if let Ok(manifest) = serde_json::from_str::<ExtensionRootManifest>(&raw) {
                    for asset in manifest.shared_assets {
                        let path = SharedAssetDeclaration::path(asset).trim().to_string();
                        if path.is_empty() {
                            continue;
                        }
                        // Never let a declaration become a sparse-checkout option or
                        // escape the owning root. The normal resolver validates it too.
                        if Path::new(&path)
                            .components()
                            .any(|part| !matches!(part, Component::Normal(_)))
                        {
                            return Err(Error::internal_io("Unsafe shared asset path", None));
                        }
                        paths.push(format!("{root}{path}"));
                    }
                }
            }
            // Cone mode includes files in every ancestor, so the owning root
            // manifest is included without selecting its entire directory.
            let mut args = vec!["sparse-checkout", "set", "--cone", "--"];
            args.extend(paths.iter().map(String::as_str));
            run(target, &args)?;
        }
        run(target, &["checkout", "--quiet", "HEAD"])?;
        Ok(())
    })();
    if let Err(error) = sparse {
        eprintln!("Sparse extension clone failed; falling back to full blobless clone: {error}");
        if target.exists() {
            std::fs::remove_dir_all(target).map_err(|error| {
                Error::internal_io(error.to_string(), Some("clean sparse clone".into()))
            })?;
        }
        git::clone_repo_at_ref_with_timeout(
            url,
            target,
            revision,
            super::EXTENSION_SOURCE_PREPARE_TIMEOUT,
        )?;
    }
    Ok(())
}
