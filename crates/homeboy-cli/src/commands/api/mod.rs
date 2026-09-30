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
    /// Make a request to a project's configured API
    Request {
        /// HTTP method (GET, POST, PUT, PATCH, DELETE)
        method: String,
        /// Project ID
        project_id: String,
        /// API endpoint (e.g., /wp/v2/posts)
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
}

#[derive(serde::Serialize)]
#[serde(untagged)]
pub enum ApiCommandOutput {
    Project(api::ApiOutput),
    Auth(Box<auth::AuthOutput>),
    Http(homeboy::core::http_request::HttpRequestOutput),
}

pub fn run(args: ApiArgs) -> CmdResult<ApiCommandOutput> {
    match args.command {
        ApiCommand::Auth(args) => map_nested(auth::run(args), |output| {
            ApiCommandOutput::Auth(Box::new(output))
        }),
        ApiCommand::Http(args) => map_nested(http::run(args), ApiCommandOutput::Http),
        ApiCommand::Request { .. } => run_project(&args.command)
            .map(|(output, code)| (ApiCommandOutput::Project(output), code)),
    }
}

fn map_nested<T>(
    result: CmdResult<T>,
    wrap: impl FnOnce(T) -> ApiCommandOutput,
) -> CmdResult<ApiCommandOutput> {
    result.map(|(output, code)| (wrap(output), code))
}

fn run_project(command: &ApiCommand) -> CmdResult<api::ApiOutput> {
    require_apply_for_mutation(command)?;
    let input = build_api_json(command);
    api::run(&input)
}

pub(crate) fn require_apply_for_mutation(command: &ApiCommand) -> homeboy::core::Result<()> {
    let ApiCommand::Request {
        method,
        project_id,
        endpoint,
        mutation,
        ..
    } = command
    else {
        // `auth` and `http` own their guards and route before project API
        // input construction is reachable.
        return Ok(());
    };

    if mutation.is_apply() || !is_mutating_method(method) {
        return Ok(());
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "apply",
        format!(
            "homeboy api request {method} sends a mutating request and requires explicit --apply. Suggested command: homeboy api request {method} {project_id} {endpoint} --apply"
        ),
        None,
        Some(vec![format!(
            "homeboy api request {method} {project_id} {endpoint} --apply"
        )]),
    ))
}

/// Every project API method except `GET` mutates, including unknown methods:
/// the guard fails closed so an unrecognized spelling can never bypass the
/// `--apply` gate. The core `api` input remains the authoritative validator
/// for the accepted method set.
fn is_mutating_method(method: &str) -> bool {
    !method.eq_ignore_ascii_case("GET")
}

fn build_api_json(command: &ApiCommand) -> String {
    let ApiCommand::Request {
        method,
        project_id,
        endpoint,
        mutation: _,
        body,
        form,
    } = command
    else {
        unreachable!("nested API commands are routed before project API input construction")
    };

    serde_json::json!({
        "projectId": project_id,
        "method": method,
        "endpoint": endpoint,
        "body": build_body(body, form),
        "bodyFormat": body_format(form),
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
