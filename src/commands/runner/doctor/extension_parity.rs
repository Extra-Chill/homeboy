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
        add_extension_show_details(stdout, &mut details);
        let copied_install = details.get("linked").map(String::as_str) == Some("false");
        let mut check = checks::ok_with_details(
            "extension.parity",
            if copied_install {
                format!(
                    "Extension '{extension_id}' resolves on the {target} runner as a copied install"
                )
            } else {
                format!("Extension '{extension_id}' resolves on the {target} runner")
            },
            details,
        );
        if copied_install {
            check.remediation = Some(format!(
                "No runner repair is needed when the copied install is current. Verify copied/current state with `{homeboy_command} extension diff-installed {extension_id}` on the runner; if stale, refresh it from source with `{homeboy_command} extension refresh <source> --id {extension_id}`."
            ));
        }
        return check;
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

fn add_extension_show_details(stdout: &str, details: &mut BTreeMap<String, String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return;
    };
    let extension = value
        .pointer("/data/extension")
        .or_else(|| value.get("extension"))
        .unwrap_or(&value);
    if let Some(linked) = extension.get("linked").and_then(|value| value.as_bool()) {
        details.insert("linked".to_string(), linked.to_string());
    }
    if let Some(path) = extension.get("path").and_then(|value| value.as_str()) {
        details.insert("path".to_string(), path.to_string());
    }
    if let Some(source_revision) = extension
        .get("source_revision")
        .and_then(|value| value.as_str())
    {
        details.insert("source_revision".to_string(), source_revision.to_string());
    }
}
