use super::*;

pub fn local_command_line(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    first_output_line(&output.stdout, &output.stderr)
}

pub fn remote_line(client: &SshClient, command: &str) -> Option<String> {
    let output = client.execute(command);
    if !output.success {
        return None;
    }
    output
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

pub fn first_output_line(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let combined = if stdout.is_empty() { stderr } else { stdout };
    String::from_utf8_lossy(combined)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

pub fn display_path(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().to_string()
}

pub fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn shell_path_expr(path: &str) -> String {
    if path == "~" {
        return "\"${HOME}\"".to_string();
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return format!("\"${{HOME}}\"/{}", shell_word(rest));
    }

    shell_word(path)
}

pub fn detail_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}
