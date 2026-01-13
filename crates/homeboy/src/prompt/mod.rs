mod types;

pub use types::*;

use crate::tty;
use std::io::{self, BufRead, Write};

/// Data-driven interactive prompt engine.
/// Handles TTY detection and provides consistent prompting behavior.
pub struct PromptEngine {
    interactive: bool,
}

impl PromptEngine {
    /// Create engine with automatic TTY detection.
    pub fn new() -> Self {
        Self {
            interactive: tty::require_tty_for_interactive(),
        }
    }

    /// Create engine with explicit interactive mode.
    pub fn with_interactive(interactive: bool) -> Self {
        Self { interactive }
    }

    /// Force non-interactive mode (useful for --yes flags).
    pub fn non_interactive() -> Self {
        Self { interactive: false }
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Run a yes/no prompt. Returns default if non-interactive.
    pub fn yes_no(&self, prompt: &YesNoPrompt) -> bool {
        if !self.interactive {
            return prompt.default;
        }

        let suffix = if prompt.default { "[Y/n]" } else { "[y/N]" };
        eprint!("{} {}: ", prompt.question, suffix);
        io::stderr().flush().ok();

        let mut input = String::new();
        if io::stdin().lock().read_line(&mut input).is_err() {
            return prompt.default;
        }

        let trimmed = input.trim().to_lowercase();
        if trimmed.is_empty() {
            return prompt.default;
        }

        trimmed.starts_with('y')
    }

    /// Display a message to stderr (only in interactive mode).
    pub fn message(&self, msg: &str) {
        if self.interactive {
            eprintln!("{}", msg);
        }
    }

    /// Run a confirm list prompt (show items, ask confirmation).
    pub fn confirm_list(&self, prompt: &ConfirmListPrompt) -> bool {
        if !self.interactive {
            return prompt.default;
        }

        eprintln!("{}", prompt.header);
        for item in &prompt.items {
            eprintln!("  {} {}", '\u{2022}', item);
        }
        eprintln!();

        self.yes_no(&YesNoPrompt {
            question: prompt.confirm_question.clone(),
            default: prompt.default,
        })
    }

    /// Run a select prompt (choose one option).
    pub fn select(&self, prompt: &SelectPrompt) -> Option<String> {
        if !self.interactive {
            return prompt
                .default_index
                .and_then(|i| prompt.options.get(i))
                .map(|o| o.value.clone());
        }

        eprintln!("{}", prompt.question);
        for (i, opt) in prompt.options.iter().enumerate() {
            let marker = if Some(i) == prompt.default_index {
                "*"
            } else {
                " "
            };
            eprintln!("  {}[{}] {}", marker, i + 1, opt.label);
        }

        eprint!("Enter choice (1-{}): ", prompt.options.len());
        io::stderr().flush().ok();

        let mut input = String::new();
        if io::stdin().lock().read_line(&mut input).is_err() {
            return prompt
                .default_index
                .and_then(|i| prompt.options.get(i))
                .map(|o| o.value.clone());
        }

        let trimmed = input.trim();
        if trimmed.is_empty() {
            return prompt
                .default_index
                .and_then(|i| prompt.options.get(i))
                .map(|o| o.value.clone());
        }

        trimmed
            .parse::<usize>()
            .ok()
            .and_then(|n| prompt.options.get(n.saturating_sub(1)))
            .map(|o| o.value.clone())
    }
}

impl Default for PromptEngine {
    fn default() -> Self {
        Self::new()
    }
}
