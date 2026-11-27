//! Tiny argv inspection helpers shared by Lab offload submodules.
//!
//! These are intentionally minimal so that `secrets`, `agent_task_bridge`, and
//! `offload` can answer "is subcommand X present?" or "is this arg
//! placeholder-empty?" without each module redefining the same parser.
//! [`flag_values`] / [`flag_value`] are the single owner of argv flag lookup
//! (`--flag value`, `--flag=value`, repeated flags, `--` passthrough stop).

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
        flag_value(self.args, start_index + 1, flag)
            .filter(|value| !value.trim().is_empty() && !value.starts_with('-'))
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
        for (insertion_index, value) in (index + 1..).zip(values) {
            self.args.insert(insertion_index, value);
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

/// Every value supplied for `flag` in `args[start_index..]`, in argv order.
///
/// Accepts both `--flag value` and `--flag=value`; repeated flags yield one
/// entry each. The token after a bare `--flag` is consumed as its value
/// verbatim, and scanning stops at the `--` passthrough separator.
pub(super) fn flag_values<'a>(args: &'a [String], start_index: usize, flag: &str) -> Vec<&'a str> {
    let flag_eq = format!("{flag}=");
    let mut values = Vec::new();
    let mut iter = args.iter().skip(start_index);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == flag {
            if let Some(value) = iter.next() {
                values.push(value.as_str());
            }
        } else if let Some(value) = arg.strip_prefix(&flag_eq) {
            values.push(value);
        }
    }
    values
}

/// The first value supplied for `flag` in `args[start_index..]`, using the
/// same forms and passthrough rules as [`flag_values`].
pub(super) fn flag_value<'a>(
    args: &'a [String],
    start_index: usize,
    flag: &str,
) -> Option<&'a str> {
    flag_values(args, start_index, flag).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn flag_values_reads_both_forms_and_repeated_flags() {
        let cases: [(&[&str], usize, Vec<&str>); 7] = [
            (&["cmd", "--ext", "a"], 0, vec!["a"]),
            (&["cmd", "--ext=a"], 0, vec!["a"]),
            (
                &["cmd", "--ext", "a", "--ext=b", "--ext", "c"],
                0,
                vec!["a", "b", "c"],
            ),
            (&["cmd", "--ext", "a", "--", "--ext", "b"], 0, vec!["a"]),
            (&["cmd", "--ext", "a", "--ext", "b"], 3, vec!["b"]),
            (&["cmd", "--ext"], 0, vec![]),
            (&["cmd", "--extension=a", "--ext-other", "b"], 0, vec![]),
        ];

        for (input, start, expected) in cases {
            let input = args(input);
            assert_eq!(flag_values(&input, start, "--ext"), expected, "{input:?}");
            assert_eq!(
                flag_value(&input, start, "--ext"),
                expected.first().copied(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn command_invocation_rejects_missing_or_flag_like_values() {
        let cases = [
            (
                args(&["homeboy", "agent-task", "cook", "--run-id", "abc"]),
                Some("abc"),
            ),
            (args(&["homeboy", "agent-task", "cook", "--run-id"]), None),
            (
                args(&["homeboy", "agent-task", "cook", "--run-id", "--other"]),
                None,
            ),
        ];

        for (input, expected) in cases {
            let invocation = CommandInvocation::for_subcommand(&input, "agent-task").unwrap();
            let action_index = invocation.child_index_matching(&["cook"]).unwrap();
            assert_eq!(
                invocation.option_value_after(action_index, "--run-id"),
                expected
            );
        }
    }

    #[test]
    fn command_invocation_detects_subcommand_before_passthrough_only() {
        let input = args(&["homeboy", "lab", "exec", "--", "agent-task", "cook"]);

        assert_eq!(
            CommandInvocation::for_subcommand(&input, "lab")
                .and_then(|invocation| invocation.child_index_matching(&["exec"])),
            Some(2)
        );
        assert!(CommandInvocation::for_subcommand(&input, "agent-task").is_none());
    }
}
