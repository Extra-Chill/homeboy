use clap::{Args, Subcommand};
use homeboy::core::server::api;

use super::utils::args::MutationArgs;
use super::CmdResult;

pub mod auth;
pub mod http;

#[derive(Args)]
pub struct ApiArgs {
    #[command(subcommand)]
    command: ApiCommand,
}

#[derive(Subcommand)]
pub(crate) enum ApiCommand {
    /// Manage API credentials and auth profiles
    Auth(auth::AuthArgs),
    /// Make generic HTTP requests to full URLs
    Http(http::HttpArgs),
    /// Make a GET request
    Get {
        /// Project ID
        project_id: String,
        /// API endpoint (e.g., /wp/v2/posts)
        endpoint: String,
    },
    /// Make a POST request
    Post {
        /// Project ID
        project_id: String,
        /// API endpoint
        endpoint: String,
        // Confirm the mutating request should be sent. Shared plan-default
        // mutation group (#11139) — `--apply` sends, bare plans.
        #[command(flatten)]
        mutation: MutationArgs,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
        /// Form field as key=value. May be repeated.
        #[arg(long)]
        form: Vec<String>,
    },
    /// Make a PUT request
    Put {
        /// Project ID
        project_id: String,
        /// API endpoint
        endpoint: String,
        // Confirm the mutating request should be sent. Shared plan-default
        // mutation group (#11139) — `--apply` sends, bare plans.
        #[command(flatten)]
        mutation: MutationArgs,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
        /// Form field as key=value. May be repeated.
        #[arg(long)]
        form: Vec<String>,
    },
    /// Make a PATCH request
    Patch {
        /// Project ID
        project_id: String,
        /// API endpoint
        endpoint: String,
        // Confirm the mutating request should be sent. Shared plan-default
        // mutation group (#11139) — `--apply` sends, bare plans.
        #[command(flatten)]
        mutation: MutationArgs,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
        /// Form field as key=value. May be repeated.
        #[arg(long)]
        form: Vec<String>,
    },
    /// Make a DELETE request
    Delete {
        /// Project ID
        project_id: String,
        /// API endpoint
        endpoint: String,
        // Confirm the mutating request should be sent. Shared plan-default
        // mutation group (#11139) — `--apply` sends, bare plans.
        #[command(flatten)]
        mutation: MutationArgs,
    },
}

#[derive(serde::Serialize)]
#[serde(untagged)]
pub enum ApiCommandOutput {
    Project(api::ApiOutput),
    Auth(auth::AuthOutput),
    Http(homeboy::core::http_request::HttpRequestOutput),
}

pub fn run(args: ApiArgs) -> CmdResult<ApiCommandOutput> {
    match args.command {
        ApiCommand::Auth(args) => map_nested(auth::run(args), ApiCommandOutput::Auth),
        ApiCommand::Http(args) => map_nested(http::run(args), ApiCommandOutput::Http),
        command => run_project(ApiArgs { command })
            .map(|(output, code)| (ApiCommandOutput::Project(output), code)),
    }
}

fn map_nested<T>(
    result: CmdResult<T>,
    wrap: impl FnOnce(T) -> ApiCommandOutput,
) -> CmdResult<ApiCommandOutput> {
    result.map(|(output, code)| (wrap(output), code))
}

fn run_project(args: ApiArgs) -> CmdResult<api::ApiOutput> {
    require_apply_for_mutation(&args)?;
    let input = build_api_json(&args);
    api::run(&input)
}

pub(crate) fn require_apply_for_mutation(args: &ApiArgs) -> homeboy::core::Result<()> {
    let Some((command, endpoint, mutation)) = mutating_command(&args.command) else {
        return Ok(());
    };

    if mutation.is_apply() {
        return Ok(());
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "apply",
        format!(
            "homeboy api {command} sends a mutating request and requires explicit --apply. Suggested command: homeboy api {command} {} {} --apply",
            project_id(&args.command).unwrap_or_default(), endpoint
        ),
        None,
        Some(vec![format!(
            "homeboy api {command} {} {} --apply",
            project_id(&args.command).unwrap_or_default(), endpoint
        )]),
    ))
}

fn mutating_command(command: &ApiCommand) -> Option<(&'static str, &str, &MutationArgs)> {
    match command {
        ApiCommand::Auth(_) | ApiCommand::Http(_) | ApiCommand::Get { .. } => None,
        ApiCommand::Post {
            endpoint, mutation, ..
        } => Some(("post", endpoint, mutation)),
        ApiCommand::Put {
            endpoint, mutation, ..
        } => Some(("put", endpoint, mutation)),
        ApiCommand::Patch {
            endpoint, mutation, ..
        } => Some(("patch", endpoint, mutation)),
        ApiCommand::Delete {
            endpoint, mutation, ..
        } => Some(("delete", endpoint, mutation)),
    }
}

fn project_id(command: &ApiCommand) -> Option<&str> {
    match command {
        ApiCommand::Get { project_id, .. }
        | ApiCommand::Post { project_id, .. }
        | ApiCommand::Put { project_id, .. }
        | ApiCommand::Patch { project_id, .. }
        | ApiCommand::Delete { project_id, .. } => Some(project_id),
        ApiCommand::Auth(_) | ApiCommand::Http(_) => None,
    }
}

fn build_api_json(args: &ApiArgs) -> String {
    let (project_id, method, endpoint, body, body_format) = match &args.command {
        ApiCommand::Get {
            project_id,
            endpoint,
        } => (project_id, "GET", endpoint.clone(), None, "json"),
        ApiCommand::Post {
            project_id,
            endpoint,
            mutation: _,
            body,
            form,
        } => (
            project_id,
            "POST",
            endpoint.clone(),
            build_body(body, form),
            body_format(form),
        ),
        ApiCommand::Put {
            project_id,
            endpoint,
            mutation: _,
            body,
            form,
        } => (
            project_id,
            "PUT",
            endpoint.clone(),
            build_body(body, form),
            body_format(form),
        ),
        ApiCommand::Patch {
            project_id,
            endpoint,
            mutation: _,
            body,
            form,
        } => (
            project_id,
            "PATCH",
            endpoint.clone(),
            build_body(body, form),
            body_format(form),
        ),
        ApiCommand::Delete {
            project_id,
            endpoint,
            mutation: _,
        } => (project_id, "DELETE", endpoint.clone(), None, "json"),
        ApiCommand::Auth(_) | ApiCommand::Http(_) => {
            unreachable!("nested API commands are routed before project API input construction")
        }
    };

    serde_json::json!({
        "projectId": project_id,
        "method": method,
        "endpoint": endpoint,
        "body": body,
        "bodyFormat": body_format,
    })
    .to_string()
}

#[cfg(test)]
#[path = "../../../../../tests/commands/api_test.rs"]
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
