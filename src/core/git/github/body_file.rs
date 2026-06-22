//! Safe Markdown body handling for `gh` CLI mutations.

use std::io::Write;

use tempfile::NamedTempFile;

use crate::core::error::{Error, Result};

const MAX_GITHUB_MARKDOWN_BODY_BYTES: usize = 60_000;
const TRUNCATION_NOTICE: &str =
    "\n\n---\n\n_Additional output omitted by Homeboy to keep the GitHub body bounded._\n";

pub(in crate::core) struct GhBodyFile {
    file: NamedTempFile,
    path: String,
}

impl GhBodyFile {
    pub(in crate::core) fn path(&self) -> &str {
        let _keep_file_alive = &self.file;
        &self.path
    }
}

pub(in crate::core) fn push_markdown_body_file_arg(
    args: &mut Vec<String>,
    body_files: &mut Vec<GhBodyFile>,
    flag: &str,
    body: &str,
) -> Result<()> {
    let body_file = write_markdown_body_file(body)?;
    args.push(flag.to_string());
    args.push(body_file.path().to_string());
    body_files.push(body_file);
    Ok(())
}

fn write_markdown_body_file(body: &str) -> Result<GhBodyFile> {
    let rendered = bounded_markdown_body(body);
    let mut file = NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            format!("Failed to create temporary GitHub body file: {}", error),
            None,
        )
    })?;
    file.write_all(rendered.as_bytes()).map_err(|error| {
        Error::internal_io(
            format!("Failed to write temporary GitHub body file: {}", error),
            file.path().to_str().map(ToString::to_string),
        )
    })?;
    let path = file.path().to_string_lossy().into_owned();
    Ok(GhBodyFile { file, path })
}

fn bounded_markdown_body(body: &str) -> String {
    if body.len() <= MAX_GITHUB_MARKDOWN_BODY_BYTES {
        return body.to_string();
    }

    let retained = MAX_GITHUB_MARKDOWN_BODY_BYTES.saturating_sub(TRUNCATION_NOTICE.len());
    let mut boundary = retained.min(body.len());
    while !body.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let mut truncated = body[..boundary].to_string();
    truncated.push_str(TRUNCATION_NOTICE);
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_file_preserves_shell_sensitive_markdown_without_argv_inline_body() {
        let body = "`homeboy-lab run`\n/home/chubes/.cache/homeboy/wp-codebox/source\n> quoted";
        let mut args = vec!["issue".to_string(), "create".to_string()];
        let mut files = Vec::new();

        push_markdown_body_file_arg(&mut args, &mut files, "--body-file", body).expect("body file");

        assert_eq!(args[2], "--body-file");
        assert_ne!(args[3], body);
        assert!(!args.iter().any(|arg| arg == body));
        let persisted = std::fs::read_to_string(files[0].path()).expect("read body file");
        assert_eq!(persisted, body);
    }

    #[test]
    fn body_file_truncates_large_bodies_on_utf8_boundary() {
        let body = format!(
            "{}{}",
            "a".repeat(MAX_GITHUB_MARKDOWN_BODY_BYTES + 500),
            " 🧑‍🍳"
        );
        let mut args = Vec::new();
        let mut files = Vec::new();

        push_markdown_body_file_arg(&mut args, &mut files, "--body-file", &body)
            .expect("body file");

        let persisted = std::fs::read_to_string(files[0].path()).expect("read body file");
        assert!(persisted.len() <= MAX_GITHUB_MARKDOWN_BODY_BYTES);
        assert!(persisted.ends_with(TRUNCATION_NOTICE));
        assert!(persisted.is_char_boundary(persisted.len()));
    }
}
