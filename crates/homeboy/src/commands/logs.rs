use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy_core::config::ConfigManager;
use homeboy_core::ssh::SshClient;
use homeboy_core::output::{print_success, print_error};

#[derive(Args)]
pub struct LogsArgs {
    #[command(subcommand)]
    command: LogsCommand,
}

#[derive(Subcommand)]
enum LogsCommand {
    /// List pinned log files
    List {
        /// Project ID
        project_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show log file content
    Show {
        /// Project ID
        project_id: String,
        /// Log file path
        path: String,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "100")]
        lines: u32,
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Clear log file contents
    Clear {
        /// Project ID
        project_id: String,
        /// Log file path
        path: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: LogsArgs) {
    match args.command {
        LogsCommand::List { project_id, json } => list(&project_id, json),
        LogsCommand::Show { project_id, path, lines, follow, json } => {
            show(&project_id, &path, lines, follow, json)
        }
        LogsCommand::Clear { project_id, path, json } => clear(&project_id, &path, json),
    }
}

fn list(project_id: &str, json: bool) {
    let project = match ConfigManager::load_project(project_id) {
        Ok(p) => p,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    if json {
        #[derive(Serialize)]
        struct LogEntry {
            path: String,
            label: Option<String>,
            #[serde(rename = "tailLines")]
            tail_lines: u32,
        }

        let logs: Vec<LogEntry> = project
            .remote_logs
            .pinned_logs
            .iter()
            .map(|l| LogEntry {
                path: l.path.clone(),
                label: l.label.clone(),
                tail_lines: l.tail_lines,
            })
            .collect();

        print_success(logs);
    } else {
        if project.remote_logs.pinned_logs.is_empty() {
            println!("No pinned logs configured for project '{}'", project_id);
            return;
        }

        println!("Pinned logs for '{}':", project_id);
        for log in &project.remote_logs.pinned_logs {
            let label = log.label.as_deref().unwrap_or(&log.path);
            println!("  - {} ({})", label, log.path);
        }
    }
}

fn show(project_id: &str, path: &str, lines: u32, follow: bool, json: bool) {
    let project = match ConfigManager::load_project(project_id) {
        Ok(p) => p,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let server_id = match &project.server_id {
        Some(id) => id,
        None => {
            let msg = format!("Server not configured for project '{}'", project_id);
            if json { print_error("SERVER_NOT_CONFIGURED", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    let server = match ConfigManager::load_server(server_id) {
        Ok(s) => s,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let full_path = resolve_log_path(path, &project.base_path);

    let client = match SshClient::from_server(&server, server_id) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error("SSH_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    if follow {
        // For follow mode, use interactive SSH
        let tail_cmd = format!("tail -f '{}'", full_path);
        let code = client.execute_interactive(Some(&tail_cmd));
        if code != 0 && code != 130 {
            std::process::exit(code);
        }
    } else {

        let command = format!("tail -n {} '{}'", lines, full_path);
        let output = client.execute(&command);

        if !output.success {
            if json {
                print_error("LOG_READ_FAILED", &output.stderr);
            } else {
                eprintln!("Error: {}", output.stderr);
            }
            return;
        }

        if json {
            #[derive(Serialize)]
            struct LogContent {
                path: String,
                lines: u32,
                content: String,
            }

            print_success(LogContent {
                path: full_path,
                lines,
                content: output.stdout,
            });
        } else {
            print!("{}", output.stdout);
        }
    }
}

fn clear(project_id: &str, path: &str, json: bool) {
    let project = match ConfigManager::load_project(project_id) {
        Ok(p) => p,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let server_id = match &project.server_id {
        Some(id) => id,
        None => {
            let msg = format!("Server not configured for project '{}'", project_id);
            if json { print_error("SERVER_NOT_CONFIGURED", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    let server = match ConfigManager::load_server(server_id) {
        Ok(s) => s,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let client = match SshClient::from_server(&server, server_id) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error("SSH_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let full_path = resolve_log_path(path, &project.base_path);
    let command = format!(": > '{}'", full_path);
    let output = client.execute(&command);

    if !output.success {
        if json {
            print_error("CLEAR_FAILED", &output.stderr);
        } else {
            eprintln!("Error: {}", output.stderr);
        }
        return;
    }

    if json {
        #[derive(Serialize)]
        struct ClearResult {
            path: String,
        }

        print_success(ClearResult { path: full_path });
    } else {
        println!("Cleared: {}", full_path);
    }
}

fn resolve_log_path(path: &str, base_path: &Option<String>) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else if let Some(base) = base_path {
        if base.is_empty() {
            path.to_string()
        } else if base.ends_with('/') {
            format!("{}{}", base, path)
        } else {
            format!("{}/{}", base, path)
        }
    } else {
        path.to_string()
    }
}
