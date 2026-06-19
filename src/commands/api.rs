use clap::{Args, Subcommand};
use homeboy::core::server::api;

use super::CmdResult;

#[derive(Args)]
pub struct ApiArgs {
    /// Project ID
    pub project_id: String,

    #[command(subcommand)]
    command: ApiCommand,
}

#[derive(Subcommand)]
enum ApiCommand {
    /// Make a GET request
    Get {
        /// API endpoint (e.g., /wp/v2/posts)
        endpoint: String,
    },
    /// Make a POST request
    Post {
        /// API endpoint
        endpoint: String,
        /// Confirm the mutating request should be sent.
        #[arg(long)]
        apply: bool,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
        /// Form field as key=value. May be repeated.
        #[arg(long)]
        form: Vec<String>,
    },
    /// Make a PUT request
    Put {
        /// API endpoint
        endpoint: String,
        /// Confirm the mutating request should be sent.
        #[arg(long)]
        apply: bool,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
        /// Form field as key=value. May be repeated.
        #[arg(long)]
        form: Vec<String>,
    },
    /// Make a PATCH request
    Patch {
        /// API endpoint
        endpoint: String,
        /// Confirm the mutating request should be sent.
        #[arg(long)]
        apply: bool,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
        /// Form field as key=value. May be repeated.
        #[arg(long)]
        form: Vec<String>,
    },
    /// Make a DELETE request
    Delete {
        /// API endpoint
        endpoint: String,
        /// Confirm the mutating request should be sent.
        #[arg(long)]
        apply: bool,
    },
}

pub fn run(args: ApiArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<api::ApiOutput> {
    require_apply_for_mutation(&args)?;
    let input = build_api_json(&args);
    api::run(&input)
}

fn require_apply_for_mutation(args: &ApiArgs) -> homeboy::core::Result<()> {
    let Some((command, endpoint, apply)) = mutating_command(&args.command) else {
        return Ok(());
    };

    if *apply {
        return Ok(());
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "apply",
        format!(
            "homeboy api {command} sends a mutating request and requires explicit --apply. Suggested command: homeboy api {} {command} {} --apply",
            args.project_id, endpoint
        ),
        None,
        Some(vec![format!(
            "homeboy api {} {command} {} --apply",
            args.project_id, endpoint
        )]),
    ))
}

fn mutating_command(command: &ApiCommand) -> Option<(&'static str, &str, &bool)> {
    match command {
        ApiCommand::Get { .. } => None,
        ApiCommand::Post {
            endpoint, apply, ..
        } => Some(("post", endpoint, apply)),
        ApiCommand::Put {
            endpoint, apply, ..
        } => Some(("put", endpoint, apply)),
        ApiCommand::Patch {
            endpoint, apply, ..
        } => Some(("patch", endpoint, apply)),
        ApiCommand::Delete { endpoint, apply } => Some(("delete", endpoint, apply)),
    }
}

fn build_api_json(args: &ApiArgs) -> String {
    let (method, endpoint, body, body_format) = match &args.command {
        ApiCommand::Get { endpoint } => ("GET", endpoint.clone(), None, "json"),
        ApiCommand::Post {
            endpoint,
            apply: _,
            body,
            form,
        } => (
            "POST",
            endpoint.clone(),
            build_body(body, form),
            body_format(form),
        ),
        ApiCommand::Put {
            endpoint,
            apply: _,
            body,
            form,
        } => (
            "PUT",
            endpoint.clone(),
            build_body(body, form),
            body_format(form),
        ),
        ApiCommand::Patch {
            endpoint,
            apply: _,
            body,
            form,
        } => (
            "PATCH",
            endpoint.clone(),
            build_body(body, form),
            body_format(form),
        ),
        ApiCommand::Delete { endpoint, apply: _ } => ("DELETE", endpoint.clone(), None, "json"),
    };

    serde_json::json!({
        "projectId": args.project_id,
        "method": method,
        "endpoint": endpoint,
        "body": body,
        "bodyFormat": body_format,
    })
    .to_string()
}

#[cfg(test)]
#[path = "../../tests/commands/api_test.rs"]
mod api_test;

fn build_body(body: &Option<String>, form: &[String]) -> Option<serde_json::Value> {
    if !form.is_empty() {
        let mut pairs = Vec::new();
        for item in form {
            if let Some((key, value)) = item.split_once('=') {
                pairs.push(serde_json::json!([key, value]));
            }
        }
        return Some(serde_json::Value::Array(pairs));
    }

    body.as_ref().and_then(|b| serde_json::from_str(b).ok())
}

fn body_format(form: &[String]) -> &'static str {
    if form.is_empty() {
        "json"
    } else {
        "form"
    }
}
