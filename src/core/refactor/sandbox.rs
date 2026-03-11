use crate::engine::temp;
use crate::Error;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct SandboxDir {
    path: PathBuf,
}

impl SandboxDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SandboxDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn clone_tree(src: &Path) -> crate::Result<SandboxDir> {
    let temp = temp::runtime_temp_dir("homeboy-refactor-ci")?;
    copy_dir_recursive(src, &temp)?;
    Ok(SandboxDir { path: temp })
}

pub fn copy_changed_files(
    src_root: &Path,
    dst_root: &Path,
    changed_files: &[String],
) -> crate::Result<()> {
    for file in changed_files {
        let src = src_root.join(file);
        let dst = dst_root.join(file);

        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(e.to_string(), Some(format!("create parent for {}", file)))
            })?;
        }

        std::fs::copy(&src, &dst).map_err(|e| {
            Error::internal_io(e.to_string(), Some(format!("copy changed file {}", file)))
        })?;
    }

    Ok(())
}

pub fn snapshot_tree(root: &str) -> crate::Result<BTreeMap<String, u64>> {
    let root_path = Path::new(root);
    let mut files = BTreeMap::new();
    snapshot_tree_recursive(root_path, root_path, &mut files)?;
    Ok(files)
}

pub fn diff_tree_snapshots(
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
) -> Vec<String> {
    let mut changed = BTreeSet::new();

    for (file, size) in after {
        if before.get(file) != Some(size) {
            changed.insert(file.clone());
        }
    }

    for file in before.keys() {
        if !after.contains_key(file) {
            changed.insert(file.clone());
        }
    }

    changed.into_iter().collect()
}

fn snapshot_tree_recursive(
    root: &Path,
    dir: &Path,
    files: &mut BTreeMap<String, u64>,
) -> crate::Result<()> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read sandbox dir".to_string())))?
    {
        let entry = entry.map_err(|e| {
            Error::internal_io(e.to_string(), Some("read sandbox entry".to_string()))
        })?;
        let path = entry.path();

        if path.is_dir() {
            snapshot_tree_recursive(root, &path, files)?;
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .map_err(|e| {
                Error::internal_io(e.to_string(), Some("strip sandbox prefix".to_string()))
            })?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = std::fs::metadata(&path).map_err(|e| {
            Error::internal_io(e.to_string(), Some("stat sandbox file".to_string()))
        })?;
        files.insert(relative, metadata.len());
    }

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> crate::Result<()> {
    std::fs::create_dir_all(dst)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create sandbox dir".to_string())))?;

    for entry in std::fs::read_dir(src)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read source dir".to_string())))?
    {
        let entry = entry
            .map_err(|e| Error::internal_io(e.to_string(), Some("read dir entry".to_string())))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                Error::internal_io(e.to_string(), Some("copy sandbox file".to_string()))
            })?;
        }
    }

    Ok(())
}
