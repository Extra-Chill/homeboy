use clap::{Args, Subcommand};
use serde::Serialize;
use std::process::Command;
use homeboy_core::config::ConfigManager;
use homeboy_core::output::{print_success, print_error};

#[derive(Args)]
pub struct GitArgs {
    #[command(subcommand)]
    command: GitCommand,
}

#[derive(Subcommand)]
enum GitCommand {
    /// Show git status for a component
    Status {
        /// Component ID
        component_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Stage all changes and commit
    Commit {
        /// Component ID
        component_id: String,
        /// Commit message
        message: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Push local commits to remote
    Push {
        /// Component ID
        component_id: String,
        /// Push tags as well
        #[arg(long)]
        tags: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Pull remote changes
    Pull {
        /// Component ID
        component_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Create a git tag
    Tag {
        /// Component ID
        component_id: String,
        /// Tag name (e.g., v0.1.2)
        tag_name: String,
        /// Tag message (creates annotated tag)
        #[arg(short, long)]
        message: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn run(args: GitArgs) {
    match args.command {
        GitCommand::Status { component_id, json } => status(&component_id, json),
        GitCommand::Commit { component_id, message, json } => commit(&component_id, &message, json),
        GitCommand::Push { component_id, tags, json } => push(&component_id, tags, json),
        GitCommand::Pull { component_id, json } => pull(&component_id, json),
        GitCommand::Tag { component_id, tag_name, message, json } => tag(&component_id, &tag_name, message.as_deref(), json),
    }
}

fn get_component_path(component_id: &str, json: bool) -> Option<String> {
    let component = match ConfigManager::load_component(component_id) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return None;
        }
    };
    Some(component.local_path)
}

fn execute_git(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
}

fn status(component_id: &str, json: bool) {
    let path = match get_component_path(component_id, json) {
        Some(p) => p,
        None => return,
    };

    if json {
        let output = match execute_git(&path, &["status", "--porcelain=v1"]) {
            Ok(o) => o,
            Err(e) => {
                print_error("GIT_ERROR", &e.to_string());
                return;
            }
        };

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct StatusResult {
            component_id: String,
            path: String,
            clean: bool,
            output: String,
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        print_success(StatusResult {
            component_id: component_id.to_string(),
            path,
            clean: stdout.trim().is_empty(),
            output: stdout,
        });
    } else {
        let status = Command::new("git")
            .args(["status"])
            .current_dir(&path)
            .status();

        if let Err(e) = status {
            eprintln!("Error: {}", e);
        }
    }
}

fn commit(component_id: &str, message: &str, json: bool) {
    let path = match get_component_path(component_id, json) {
        Some(p) => p,
        None => return,
    };

    // Check if there are changes to commit
    let status_output = match execute_git(&path, &["status", "--porcelain=v1"]) {
        Ok(o) => o,
        Err(e) => {
            if json { print_error("GIT_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let status_str = String::from_utf8_lossy(&status_output.stdout);
    if status_str.trim().is_empty() {
        if json { print_error("NO_CHANGES", "Nothing to commit, working tree clean"); }
        else { println!("Nothing to commit, working tree clean"); }
        return;
    }

    // Stage all changes
    let add_output = match execute_git(&path, &["add", "."]) {
        Ok(o) => o,
        Err(e) => {
            if json { print_error("GIT_ERROR", &e.to_string()); }
            else { eprintln!("Error staging files: {}", e); }
            return;
        }
    };

    if !add_output.status.success() {
        let stderr = String::from_utf8_lossy(&add_output.stderr).to_string();
        if json { print_error("GIT_ADD_FAILED", &stderr); }
        else { eprintln!("Error staging files: {}", stderr); }
        return;
    }

    // Commit
    let commit_output = match execute_git(&path, &["commit", "-m", message]) {
        Ok(o) => o,
        Err(e) => {
            if json { print_error("GIT_ERROR", &e.to_string()); }
            else { eprintln!("Error committing: {}", e); }
            return;
        }
    };

    if json {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CommitResult {
            component_id: String,
            success: bool,
            message: String,
            output: String,
        }

        let output_str = if commit_output.status.success() {
            String::from_utf8_lossy(&commit_output.stdout).to_string()
        } else {
            String::from_utf8_lossy(&commit_output.stderr).to_string()
        };

        if commit_output.status.success() {
            print_success(CommitResult {
                component_id: component_id.to_string(),
                success: true,
                message: message.to_string(),
                output: output_str,
            });
        } else {
            print_error("GIT_COMMIT_FAILED", &output_str);
        }
    } else {
        if commit_output.status.success() {
            print!("{}", String::from_utf8_lossy(&commit_output.stdout));
        } else {
            eprint!("{}", String::from_utf8_lossy(&commit_output.stderr));
        }
    }
}

fn push(component_id: &str, tags: bool, json: bool) {
    let path = match get_component_path(component_id, json) {
        Some(p) => p,
        None => return,
    };

    let push_args: Vec<&str> = if tags {
        vec!["push", "--tags"]
    } else {
        vec!["push"]
    };

    if json {
        let output = match execute_git(&path, &push_args) {
            Ok(o) => o,
            Err(e) => {
                print_error("GIT_ERROR", &e.to_string());
                return;
            }
        };

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PushResult {
            component_id: String,
            success: bool,
            tags_pushed: bool,
            output: String,
        }

        // git push outputs progress to stderr
        let output_str = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            print_success(PushResult {
                component_id: component_id.to_string(),
                success: true,
                tags_pushed: tags,
                output: output_str,
            });
        } else {
            print_error("GIT_PUSH_FAILED", &output_str);
        }
    } else {
        let status = Command::new("git")
            .args(&push_args)
            .current_dir(&path)
            .status();

        if let Err(e) = status {
            eprintln!("Error: {}", e);
        }
    }
}

fn pull(component_id: &str, json: bool) {
    let path = match get_component_path(component_id, json) {
        Some(p) => p,
        None => return,
    };

    if json {
        let output = match execute_git(&path, &["pull"]) {
            Ok(o) => o,
            Err(e) => {
                print_error("GIT_ERROR", &e.to_string());
                return;
            }
        };

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PullResult {
            component_id: String,
            success: bool,
            output: String,
        }

        let output_str = if output.status.success() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).to_string()
        };

        if output.status.success() {
            print_success(PullResult {
                component_id: component_id.to_string(),
                success: true,
                output: output_str,
            });
        } else {
            print_error("GIT_PULL_FAILED", &output_str);
        }
    } else {
        let status = Command::new("git")
            .args(["pull"])
            .current_dir(&path)
            .status();

        if let Err(e) = status {
            eprintln!("Error: {}", e);
        }
    }
}

fn tag(component_id: &str, tag_name: &str, message: Option<&str>, json: bool) {
    let path = match get_component_path(component_id, json) {
        Some(p) => p,
        None => return,
    };

    let tag_args: Vec<&str> = match message {
        Some(msg) => vec!["tag", "-a", tag_name, "-m", msg],
        None => vec!["tag", tag_name],
    };

    let output = match execute_git(&path, &tag_args) {
        Ok(o) => o,
        Err(e) => {
            if json { print_error("GIT_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    if json {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct TagResult {
            component_id: String,
            success: bool,
            tag_name: String,
            annotated: bool,
        }

        if output.status.success() {
            print_success(TagResult {
                component_id: component_id.to_string(),
                success: true,
                tag_name: tag_name.to_string(),
                annotated: message.is_some(),
            });
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            print_error("GIT_TAG_FAILED", &stderr);
        }
    } else {
        if output.status.success() {
            println!("Created tag: {}", tag_name);
        } else {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
    }
}
