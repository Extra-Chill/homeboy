use clap::Args;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::docs;

#[derive(Args)]
pub struct DocsArgs {
    /// Topic to filter (e.g., 'deploy', 'project set')
    #[arg(trailing_var_arg = true)]
    topic: Vec<String>,
}

pub fn run(args: DocsArgs) {
    let (topic_label, content) = docs::resolve(&args.topic);

    if content.is_empty() {
        let search_topic = args.topic.join(" ");
        eprintln!("No documentation found for '{}'.", search_topic);
        eprintln!("Available topics: {}", docs::available_topics());
        return;
    }

    if args.topic.is_empty() {
        display_with_pager(content);
        return;
    }

    if topic_label == "index" {
        display_with_pager(content);
        return;
    }

    println!("{}", content);
}

fn display_with_pager(content: &str) {
    // Check if stdout is a terminal
    if !atty::is(atty::Stream::Stdout) {
        println!("{}", content);
        return;
    }

    // Try to use less pager
    let less = Command::new("less")
        .arg("-R")
        .stdin(Stdio::piped())
        .spawn();

    match less {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(content.as_bytes());
            }
            let _ = child.wait();
        }
        Err(_) => {
            // Fallback to plain output
            println!("{}", content);
        }
    }
}

