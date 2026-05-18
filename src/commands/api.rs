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
    },
}

pub fn run(args: ApiArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<api::ApiOutput> {
    let input = build_api_json(&args);
    api::run(&input)
}

fn build_api_json(args: &ApiArgs) -> String {
    let (method, endpoint, body, body_format) = match &args.command {
        ApiCommand::Get { endpoint } => ("GET", endpoint.clone(), None, "json"),
        ApiCommand::Post {
            endpoint,
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
            body,
            form,
        } => (
            "PATCH",
            endpoint.clone(),
            build_body(body, form),
            body_format(form),
        ),
        ApiCommand::Delete { endpoint } => ("DELETE", endpoint.clone(), None, "json"),
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
