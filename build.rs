use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let docs_root = manifest_dir.join("docs");

    if !docs_root.exists() {
        panic!("Docs directory not found: {}", docs_root.display());
    }

    let mut doc_paths = Vec::new();
    collect_md_files(&docs_root, &docs_root, &mut doc_paths);
    doc_paths.sort();

    for path in &doc_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let generated = generate_docs_rs(&docs_root, &doc_paths);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR missing"));
    fs::write(out_dir.join("generated_docs.rs"), generated)
        .expect("Failed to write generated_docs.rs");

    emit_git_build_identity(&manifest_dir);
}

fn emit_git_build_identity(manifest_dir: &Path) {
    let git_dir = resolve_git_dir(manifest_dir).unwrap_or_else(|| manifest_dir.join(".git"));
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    if let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }

    if let Some(commit) = git_output(manifest_dir, &["rev-parse", "--short=12", "HEAD"]) {
        println!("cargo:rustc-env=HOMEBOY_BUILD_GIT_COMMIT={commit}");
    }
    if let Some(status) = git_output(manifest_dir, &["status", "--porcelain"]) {
        println!(
            "cargo:rustc-env=HOMEBOY_BUILD_GIT_DIRTY={}",
            if status.trim().is_empty() {
                "false"
            } else {
                "true"
            }
        );
    }
}

fn resolve_git_dir(manifest_dir: &Path) -> Option<PathBuf> {
    let git_path = manifest_dir.join(".git");
    if git_path.is_dir() {
        return Some(git_path);
    }

    let git_file = fs::read_to_string(&git_path).ok()?;
    let raw_path = git_file.trim().strip_prefix("gitdir: ")?.trim();
    let path = PathBuf::from(raw_path);
    Some(if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path)
    })
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn collect_md_files(docs_root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("Failed to read dir {}: {}", dir.display(), err));

    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("Failed to read dir entry: {}", err));
        let path = entry.path();

        if path.is_dir() {
            collect_md_files(docs_root, &path, out);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            // Exclude docs/changelog.md from embedded docs.
            // Homeboy's changelog is accessed via `homeboy changelog --self` instead.
            // Only exclude changelog.md at the docs root, not docs/commands/changelog.md.
            if path.file_name().and_then(|n| n.to_str()) == Some("changelog.md")
                && path.parent() == Some(docs_root)
            {
                continue;
            }
            out.push(path);
        }
    }
}

fn generate_docs_rs(docs_root: &Path, doc_paths: &[PathBuf]) -> String {
    let mut out = String::new();
    out.push_str("pub static GENERATED_DOCS: &[(&str, &str)] = &[\n");

    for path in doc_paths {
        let key = key_for_path(docs_root, path);
        let content = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("Failed to read doc {}: {}", path.display(), err));

        out.push_str("    (\"");
        out.push_str(&escape_rust_string(&key));
        out.push_str("\", r###\"");
        out.push_str(&content);
        out.push_str("\"###),\n");
    }

    out.push_str("];\n");
    out
}

fn key_for_path(docs_root: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(docs_root)
        .unwrap_or_else(|_| panic!("Doc path is not under docs: {}", path.display()));

    let mut key = relative.to_string_lossy().replace('\\', "/");

    if let Some(without_ext) = key.strip_suffix(".md") {
        key = without_ext.to_string();
    }

    if key == "index" {
        return "index".to_string();
    }

    key
}

fn escape_rust_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}
