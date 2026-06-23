//! Render `homeboy review`/tunnel-service fallback command hints when a runner
//! cannot offload a scoped command directly.

pub(crate) struct ReviewLabFallbackCommands {
    pub(crate) audit: String,
    pub(crate) lint: String,
    pub(crate) test: String,
}

pub(crate) fn review_lab_fallback_commands(
    runner_id: &str,
    normalized_args: &[String],
) -> Option<ReviewLabFallbackCommands> {
    let review_index = normalized_args.iter().position(|arg| arg == "review")?;
    let review_args = &normalized_args[review_index + 1..];
    let mut component: Option<&str> = None;
    let mut path: Option<&str> = None;
    let mut extensions: Vec<&str> = Vec::new();
    let mut scoped = false;
    let mut i = 0;

    while i < review_args.len() {
        let arg = review_args[i].as_str();
        if arg == "--changed-only" || arg.starts_with("--changed-since=") {
            scoped = true;
        } else if arg == "--changed-since" {
            scoped = true;
            i += 1;
        } else if arg == "--path" {
            if let Some(value) = review_args.get(i + 1) {
                path = Some(value.as_str());
            }
            i += 1;
        } else if let Some(value) = arg.strip_prefix("--path=") {
            path = Some(value);
        } else if arg == "--extension" {
            if let Some(value) = review_args.get(i + 1) {
                extensions.push(value.as_str());
            }
            i += 1;
        } else if let Some(value) = arg.strip_prefix("--extension=") {
            extensions.push(value);
        } else if !arg.starts_with('-') && component.is_none() {
            component = Some(arg);
        }
        i += 1;
    }

    if !scoped {
        return None;
    }

    let mut common = vec!["--runner".to_string(), shell_arg(runner_id)];
    if let Some(path) = path {
        common.push("--path".to_string());
        common.push(shell_arg(path));
    }
    for extension in extensions {
        common.push("--extension".to_string());
        common.push(shell_arg(extension));
    }
    if path.is_none() {
        if let Some(component) = component {
            common.push(shell_arg(component));
        }
    }

    Some(ReviewLabFallbackCommands {
        audit: fallback_command("audit", &common),
        lint: fallback_command("lint", &common),
        test: fallback_command("test", &common),
    })
}

pub(crate) fn fallback_command(command: &str, args: &[String]) -> String {
    let suffix = if args.is_empty() {
        String::new()
    } else {
        format!(" {}", args.join(" "))
    };
    format!("`homeboy {command}{suffix}`")
}

pub(crate) fn shell_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) fn tunnel_service_command(normalized_args: &[String]) -> Option<&str> {
    normalized_args.windows(3).find_map(|window| {
        let [first, second, third] = window else {
            return None;
        };
        if first == "tunnel" && second == "service" {
            match third.as_str() {
                "list" | "show" | "status" | "url" | "set" | "remove" => Some(third.as_str()),
                _ => None,
            }
        } else {
            None
        }
    })
}
