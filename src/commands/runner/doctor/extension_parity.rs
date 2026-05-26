use std::collections::BTreeMap;
use std::process::Command;

use homeboy::core::server::SshClient;

use super::types::RunnerCheck;
use super::{checks, common};

pub fn local_check(homeboy_command: &str, cwd: Option<&str>, extension_id: &str) -> RunnerCheck {
    let mut command = Command::new(homeboy_command);
    command.args(["extension", "show", extension_id]);
    if let Some(cwd) = cwd.filter(|path| !path.trim().is_empty()) {
        command.current_dir(cwd);
    }

    match command.output() {
        Ok(output) => check_from_probe(
            "local",
            homeboy_command,
            cwd,
            extension_id,
            output.status.success(),
            &String::from_utf8_lossy(&output.stderr),
            &String::from_utf8_lossy(&output.stdout),
        ),
        Err(err) => check_from_probe(
            "local",
            homeboy_command,
            cwd,
            extension_id,
            false,
            &err.to_string(),
            "",
        ),
    }
}

pub fn remote_check(
    client: &SshClient,
    homeboy_command: &str,
    cwd: Option<&str>,
    extension_id: &str,
) -> RunnerCheck {
    let show_command = format!(
        "{} extension show {}",
        common::shell_word(homeboy_command),
        common::shell_word(extension_id)
    );
    let command = if let Some(cwd) = cwd.filter(|path| !path.trim().is_empty()) {
        format!("cd {} && {show_command}", common::shell_word(cwd))
    } else {
        show_command
    };
    let output = client.execute(&command);

    check_from_probe(
        "remote",
        homeboy_command,
        cwd,
        extension_id,
        output.success,
        &output.stderr,
        &output.stdout,
    )
}

pub fn check_from_probe(
    target: &str,
    homeboy_command: &str,
    cwd: Option<&str>,
    extension_id: &str,
    success: bool,
    stderr: &str,
    stdout: &str,
) -> RunnerCheck {
    let mut details = BTreeMap::new();
    details.insert("extension_id".to_string(), extension_id.to_string());
    details.insert(
        "command".to_string(),
        format!("{homeboy_command} extension show {extension_id}"),
    );
    if let Some(cwd) = cwd.filter(|path| !path.trim().is_empty()) {
        details.insert("cwd".to_string(), cwd.to_string());
    }

    if success {
        return checks::ok_with_details(
            "extension.parity",
            format!("Extension '{extension_id}' resolves on the {target} runner"),
            details,
        );
    }

    let diagnostics = diagnostic_tail(stderr, stdout);
    if !diagnostics.is_empty() {
        details.insert("diagnostics".to_string(), diagnostics);
    }

    checks::error(
        "extension.parity",
        format!("Extension '{extension_id}' does not resolve on the {target} runner"),
        Some(format!(
            "Install the extension on the runner before offloading: {homeboy_command} extension install <source> --id {extension_id}"
        )),
        details,
    )
}

fn diagnostic_tail(stderr: &str, stdout: &str) -> String {
    let output = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    if let Some(message) = json_error_message(output) {
        return message;
    }
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

fn json_error_message(output: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(output.trim()).ok()?;
    value
        .pointer("/error/message")
        .or_else(|| value.pointer("/data/error/message"))
        .or_else(|| value.get("message"))
        .and_then(|message| message.as_str())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
}
