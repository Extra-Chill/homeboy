use super::*;
use types::{RunnerCheck, RunnerDoctorStatus, ToolProbe};

pub fn tool_check(spec: RunnerToolSpec, probe: &ToolProbe) -> RunnerCheck {
    if probe.available {
        ok(
            spec.check_id,
            format!("{} is available", spec.command),
            None,
        )
    } else if spec.required {
        error(
            spec.check_id,
            format!("{} is required but was not found", spec.command),
            Some(spec.remediation.to_string()),
            BTreeMap::new(),
        )
    } else {
        warning(
            spec.check_id,
            format!("{} was not found", spec.command),
            Some(spec.remediation.to_string()),
        )
    }
}

pub fn required_tool_check(command: &str, probe: &ToolProbe) -> RunnerCheck {
    let mut details = BTreeMap::new();
    details.insert("command".to_string(), command.to_string());
    if let Some(path) = &probe.path {
        details.insert("path".to_string(), path.clone());
    }

    if probe.available {
        ok_with_details(
            format!("tool.required.{command}"),
            format!("Required runner tool {command} is available"),
            details,
        )
    } else {
        error(
            format!("tool.required.{command}"),
            format!("Required runner tool {command} was not found"),
            Some(format!(
                "Install {command} on the runner and ensure it is on PATH, or remove it from the provider preflight requirements"
            )),
            details,
        )
    }
}

pub fn playwright_check(playwright: bool, browser_ready: bool) -> RunnerCheck {
    match (playwright, browser_ready) {
        (true, true) => ok(
            "playwright.browser_ready",
            "Playwright CLI and browser cache are detectable".to_string(),
            None,
        ),
        (true, false) => warning(
            "playwright.browser_ready",
            "Playwright CLI is available but browser readiness was not detected".to_string(),
            Some(
                "Run `playwright install` in the relevant project if browser traces fail"
                    .to_string(),
            ),
        ),
        (false, true) => warning(
            "playwright.browser_ready",
            "Browser cache is present but Playwright CLI was not found".to_string(),
            Some("Install Playwright CLI in the runner environment".to_string()),
        ),
        (false, false) => warning(
            "playwright.browser_ready",
            "Playwright/browser readiness was not detected".to_string(),
            Some(
                "Install Playwright and browser binaries for browser-backed traces".to_string(),
            ),
        ),
    }
}

pub fn headed_browser_check(
    headed_browser_ready: bool,
    display_ready: bool,
    xvfb_ready: bool,
) -> RunnerCheck {
    let mut details = BTreeMap::new();
    details.insert("display_ready".to_string(), display_ready.to_string());
    details.insert("xvfb_ready".to_string(), xvfb_ready.to_string());
    if headed_browser_ready {
        ok_with_details(
            "browser.headed_ready",
            "Display or Xvfb support is available for headed browser workloads".to_string(),
            details,
        )
    } else {
        warning_with_details(
            "browser.headed_ready",
            "No DISPLAY or Xvfb support was detected for headed browser workloads".to_string(),
            Some("Install xvfb/xvfb-run on Linux runners or run Electron/Chromium workloads in headless/Ozone mode".to_string()),
            details,
        )
    }
}

pub fn path_writable_check(
    id: &'static str,
    writable: bool,
    path: &Path,
    remediation: &str,
) -> RunnerCheck {
    let mut details = BTreeMap::new();
    details.insert("path".to_string(), common::display_path(path));
    if writable {
        ok_with_details(
            id,
            "Path is writable by the runner user".to_string(),
            details,
        )
    } else {
        error(
            id,
            "Path is not writable by the runner user".to_string(),
            Some(remediation.to_string()),
            details,
        )
    }
}

pub fn homeboy_version_skew_check(
    local_version: &str,
    remote_version: &str,
    runner_id: &str,
    server_id: &str,
) -> Option<RunnerCheck> {
    let local_version = local_version.trim();
    let remote_version = remote_version.trim();
    if local_version.is_empty()
        || remote_version.is_empty()
        || remote_version == "unknown"
        || local_version == remote_version
    {
        return None;
    }

    let mut details = BTreeMap::new();
    details.insert("local_version".to_string(), local_version.to_string());
    details.insert("remote_version".to_string(), remote_version.to_string());
    Some(warning_with_details(
        "homeboy.version_skew",
        format!(
            "Local Homeboy {local_version} differs from remote runner Homeboy {remote_version}"
        ),
        Some(format!(
            "Upgrade Homeboy on runner `{runner_id}` to match the local client; for example: `homeboy ssh {server_id} -- homeboy upgrade --no-restart`, or rerun the runner setup/upgrade workflow"
        )),
        details,
    ))
}

pub fn ok(id: impl Into<String>, message: String, remediation: Option<String>) -> RunnerCheck {
    RunnerCheck {
        id: id.into(),
        status: RunnerDoctorStatus::Ok,
        message,
        remediation,
        details: BTreeMap::new(),
    }
}

pub fn warning(
    id: impl Into<String>,
    message: String,
    remediation: Option<String>,
) -> RunnerCheck {
    RunnerCheck {
        id: id.into(),
        status: RunnerDoctorStatus::Warning,
        message,
        remediation,
        details: BTreeMap::new(),
    }
}

pub(super) fn warning_with_details(
    id: impl Into<String>,
    message: String,
    remediation: Option<String>,
    details: BTreeMap<String, String>,
) -> RunnerCheck {
    RunnerCheck {
        id: id.into(),
        status: RunnerDoctorStatus::Warning,
        message,
        remediation,
        details,
    }
}

pub fn error(
    id: impl Into<String>,
    message: String,
    remediation: Option<String>,
    details: BTreeMap<String, String>,
) -> RunnerCheck {
    RunnerCheck {
        id: id.into(),
        status: RunnerDoctorStatus::Error,
        message,
        remediation,
        details,
    }
}

pub fn overall_status(checks: &[RunnerCheck]) -> RunnerDoctorStatus {
    if checks
        .iter()
        .any(|check| check.status == RunnerDoctorStatus::Error)
    {
        RunnerDoctorStatus::Error
    } else if checks
        .iter()
        .any(|check| check.status == RunnerDoctorStatus::Warning)
    {
        RunnerDoctorStatus::Warning
    } else {
        RunnerDoctorStatus::Ok
    }
}

pub(super) fn ok_with_details(
    id: impl Into<String>,
    message: String,
    details: BTreeMap<String, String>,
) -> RunnerCheck {
    RunnerCheck {
        id: id.into(),
        status: RunnerDoctorStatus::Ok,
        message,
        remediation: None,
        details,
    }
}
