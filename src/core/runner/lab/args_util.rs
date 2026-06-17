//! Tiny argv inspection helpers shared by Lab offload submodules.
//!
//! These are intentionally minimal so that `secrets`, `agent_task_bridge`, and
//! `offload` can answer "is subcommand X present?" or "is this arg
//! placeholder-empty?" without each module redefining the same parser.

pub(super) fn subcommand_index(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter().position(|arg| arg == subcommand)
}

pub(super) fn non_empty_arg(value: &str) -> Option<String> {
    (!value.trim().is_empty() && !value.starts_with('-')).then(|| value.to_string())
}

pub(super) struct CommandInvocation<'a> {
    args: &'a [String],
    command_index: usize,
}

impl<'a> CommandInvocation<'a> {
    pub(super) fn for_subcommand(args: &'a [String], command: &str) -> Option<Self> {
        subcommand_index_before_passthrough(args, command).map(|command_index| Self {
            args,
            command_index,
        })
    }

    pub(super) fn child_index_matching(&self, values: &[&str]) -> Option<usize> {
        self.args
            .get(self.command_index + 1)
            .filter(|arg| values.contains(&arg.as_str()))
            .map(|_| self.command_index + 1)
    }

    pub(super) fn option_value_after(&self, start_index: usize, flag: &str) -> Option<&'a str> {
        option_value(self.args, start_index + 1, flag)
    }
}

pub(super) struct ArgEditor {
    args: Vec<String>,
}

impl ArgEditor {
    pub(super) fn new(args: &[String]) -> Self {
        Self {
            args: args.to_vec(),
        }
    }

    pub(super) fn insert_after(
        mut self,
        index: usize,
        values: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut insertion_index = index + 1;
        for value in values {
            self.args.insert(insertion_index, value);
            insertion_index += 1;
        }
        self
    }

    pub(super) fn into_args(self) -> Vec<String> {
        self.args
    }
}

fn subcommand_index_before_passthrough(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .position(|arg| arg == subcommand)
}

fn option_value<'a>(args: &'a [String], start_index: usize, flag: &str) -> Option<&'a str> {
    let flag_eq = format!("{flag}=");
    let mut iter = args.iter().enumerate().skip(start_index);
    while let Some((index, arg)) = iter.next() {
        if arg == "--" {
            return None;
        }
        if let Some(value) = arg.strip_prefix(&flag_eq) {
            return (!value.is_empty()).then_some(value);
        }
        if arg == flag {
            return args
                .get(index + 1)
                .filter(|value| !value.trim().is_empty() && !value.starts_with('-'))
                .map(String::as_str);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn command_invocation_reads_flag_value_forms() {
        let cases = [
            (
                args(&["homeboy", "agent-task", "dispatch", "--run-id", "abc"]),
                Some("abc"),
            ),
            (
                args(&["homeboy", "agent-task", "dispatch", "--run-id=abc"]),
                Some("abc"),
            ),
            (
                args(&["homeboy", "agent-task", "dispatch", "--run-id"]),
                None,
            ),
            (
                args(&["homeboy", "agent-task", "dispatch", "--run-id", "--other"]),
                None,
            ),
            (
                args(&["homeboy", "agent-task", "dispatch", "--", "--run-id", "abc"]),
                None,
            ),
        ];

        for (input, expected) in cases {
            let invocation = CommandInvocation::for_subcommand(&input, "agent-task").unwrap();
            let action_index = invocation
                .child_index_matching(&["dispatch", "cook"])
                .unwrap();
            assert_eq!(
                invocation.option_value_after(action_index, "--run-id"),
                expected
            );
        }
    }

    #[test]
    fn arg_editor_inserts_after_subcommand_child() {
        let input = args(&["homeboy", "agent-task", "dispatch", "--repo", "homeboy"]);
        let invocation = CommandInvocation::for_subcommand(&input, "agent-task").unwrap();
        let action_index = invocation
            .child_index_matching(&["dispatch", "cook"])
            .unwrap();

        let edited = ArgEditor::new(&input)
            .insert_after(
                action_index,
                ["--run-id".to_string(), "generated".to_string()],
            )
            .into_args();

        assert_eq!(
            edited,
            args(&[
                "homeboy",
                "agent-task",
                "dispatch",
                "--run-id",
                "generated",
                "--repo",
                "homeboy",
            ])
        );
    }

    #[test]
    fn command_invocation_detects_subcommand_before_passthrough_only() {
        let input = args(&["homeboy", "lab", "exec", "--", "agent-task", "dispatch"]);

        assert_eq!(
            CommandInvocation::for_subcommand(&input, "lab")
                .and_then(|invocation| invocation.child_index_matching(&["exec"])),
            Some(2)
        );
        assert!(CommandInvocation::for_subcommand(&input, "agent-task").is_none());
    }
}
