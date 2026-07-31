//! Typed command values and their argument-source provenance.
//!
//! Clap owns parser mechanics. This module turns its ephemeral `ArgMatches`
//! sources into a serializable contract that policy and execution layers can
//! validate or persist without depending on Clap.

use clap::{parser::ValueSource, ArgMatches};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentSource {
    CommandLine,
    Environment,
    Configuration,
    Policy,
    Generated,
    Default,
}

impl ArgumentSource {
    fn from_clap(source: ValueSource) -> Self {
        match source {
            ValueSource::CommandLine => Self::CommandLine,
            ValueSource::EnvVariable => Self::Environment,
            ValueSource::DefaultValue => Self::Default,
            _ => Self::Default,
        }
    }
}

/// Serializable source map for a parsed command. Keys are canonical Clap
/// argument IDs, so aliases share their declared argument's provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandArgumentProvenance(BTreeMap<String, ArgumentSource>);

impl CommandArgumentProvenance {
    pub fn from_matches(matches: &ArgMatches) -> Self {
        let mut provenance = Self::default();
        provenance.collect(matches);
        provenance
    }

    /// Records a non-parser resolution (configuration, policy, or generated
    /// value) after command compilation.
    pub fn set(&mut self, argument: impl Into<String>, source: ArgumentSource) {
        self.0.insert(argument.into(), source);
    }

    pub fn source(&self, argument: &str) -> Option<ArgumentSource> {
        self.0.get(argument).copied()
    }

    /// Rejects a source policy before a caller starts workspace, provider, or
    /// other external work. A single policy can cover an argument group.
    pub fn require_sources(
        &self,
        arguments: &[&str],
        allowed: &[ArgumentSource],
    ) -> Result<(), ArgumentSourcePolicyError> {
        let rejected: Vec<_> = arguments
            .iter()
            .filter_map(|argument| {
                let source = self.source(argument)?;
                (!allowed.contains(&source)).then(|| ArgumentSourceViolation {
                    argument: (*argument).to_string(),
                    source,
                })
            })
            .collect();
        let missing: Vec<_> = arguments
            .iter()
            .filter(|argument| self.source(argument).is_none())
            .map(|argument| (*argument).to_string())
            .collect();

        if rejected.is_empty() && missing.is_empty() {
            Ok(())
        } else {
            Err(ArgumentSourcePolicyError { rejected, missing })
        }
    }

    /// Adds durable provenance to a plan or evidence JSON object.
    pub fn project_into(&self, value: &mut serde_json::Value) {
        if !value.is_object() {
            *value = serde_json::json!({});
        }
        value["command_argument_provenance"] =
            serde_json::to_value(self).expect("argument provenance serializes");
    }

    fn collect(&mut self, matches: &ArgMatches) {
        for id in matches.ids() {
            // `ids()` yields ids that `value_source` cannot resolve, and it
            // PANICS on them rather than returning `None`:
            //
            //   `"placement"` is not an id of an argument or a group.
            //
            // The runtime parses an extension-augmented command, whose global
            // and flattened argument layout surfaces such ids, so any command
            // reaching this collector aborted the process. `homeboy refactor
            // rename` did exactly that. `try_get_raw` reports the same
            // distinction as a recoverable error, so use it as the guard.
            if matches.try_get_raw(id.as_str()).is_err() {
                continue;
            }
            if let Some(source) = matches.value_source(id.as_str()) {
                self.set(id.as_str(), ArgumentSource::from_clap(source));
            }
        }
        if let Some((_, child)) = matches.subcommand() {
            self.collect(child);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentSourceViolation {
    pub argument: String,
    pub source: ArgumentSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentSourcePolicyError {
    pub rejected: Vec<ArgumentSourceViolation>,
    pub missing: Vec<String>,
}

/// A parsed typed command plus the sources used to compile it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCommand<T> {
    pub value: T,
    pub provenance: CommandArgumentProvenance,
}

impl<T> CompiledCommand<T> {
    pub fn new(value: T, provenance: CommandArgumentProvenance) -> Self {
        Self { value, provenance }
    }

    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> CompiledCommand<U> {
        CompiledCommand {
            value: map(self.value),
            provenance: self.provenance,
        }
    }
}

/// Adapter for policy compilers such as tracker-resolved Cook (#10889). The
/// adapter deliberately takes already-typed values: tracker resolution owns
/// how values are derived, while this shared contract owns their provenance.
pub struct TrackerCookArgumentAdapter;

impl TrackerCookArgumentAdapter {
    pub fn compile<T>(
        value: T,
        sources: impl IntoIterator<Item = (&'static str, ArgumentSource)>,
    ) -> CompiledCommand<T> {
        let mut provenance = CommandArgumentProvenance::default();
        for (argument, source) in sources {
            provenance.set(argument, source);
        }
        CompiledCommand::new(value, provenance)
    }

    /// Tracker policy can reject caller overrides before it provisions a
    /// worktree or discovers a provider.
    pub fn require_policy_owned(
        provenance: &CommandArgumentProvenance,
        arguments: &[&str],
    ) -> Result<(), ArgumentSourcePolicyError> {
        provenance.require_sources(
            arguments,
            &[
                ArgumentSource::Configuration,
                ArgumentSource::Policy,
                ArgumentSource::Generated,
                ArgumentSource::Default,
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

    #[derive(Args)]
    struct FlattenedArgs {
        #[arg(long, default_value_t = 3)]
        max_attempts: u32,
    }

    #[derive(Args)]
    struct NestedArgs {
        #[command(flatten)]
        flattened: FlattenedArgs,
        #[arg(long, env = "HOMEBOY_PROVENANCE_TEST_ENV")]
        from_env: Option<String>,
        #[arg(long, visible_alias = "base-branch", default_value = "main")]
        base: String,
        #[arg(long, hide = true)]
        compatibility: bool,
        #[arg(long)]
        labels: Vec<String>,
    }

    #[derive(Subcommand)]
    enum TestCommand {
        Cook(NestedArgs),
    }

    #[derive(Parser)]
    struct TestCli {
        #[arg(long, global = true, default_value = "auto")]
        placement: String,
        #[command(subcommand)]
        command: TestCommand,
    }

    #[test]
    fn captures_parser_sources_through_nested_flattened_global_alias_hidden_and_repeated_arguments()
    {
        let matches = TestCli::command()
            .try_get_matches_from([
                "homeboy",
                "--placement",
                "local",
                "cook",
                "--max-attempts",
                "3",
                "--base-branch",
                "main",
                "--compatibility",
                "--labels",
                "one",
                "--labels",
                "two",
            ])
            .expect("parse command");
        let typed = TestCli::from_arg_matches(&matches).expect("typed command");
        let compiled =
            CompiledCommand::new(typed, CommandArgumentProvenance::from_matches(&matches));

        assert_eq!(
            compiled.provenance.source("placement"),
            Some(ArgumentSource::CommandLine)
        );
        assert_eq!(
            compiled.provenance.source("max_attempts"),
            Some(ArgumentSource::CommandLine)
        );
        assert_eq!(
            compiled.provenance.source("base"),
            Some(ArgumentSource::CommandLine)
        );
        assert_eq!(
            compiled.provenance.source("compatibility"),
            Some(ArgumentSource::CommandLine)
        );
        assert_eq!(
            compiled.provenance.source("labels"),
            Some(ArgumentSource::CommandLine)
        );
        let TestCommand::Cook(args) = compiled.value.command;
        assert_eq!(args.labels, ["one", "two"]);
    }

    #[test]
    fn keeps_implicit_and_explicit_default_values_distinct() {
        let implicit = TestCli::command()
            .try_get_matches_from(["homeboy", "cook"])
            .expect("parse defaults");
        let explicit = TestCli::command()
            .try_get_matches_from(["homeboy", "cook", "--max-attempts", "3"])
            .expect("parse explicit default");

        assert_eq!(
            CommandArgumentProvenance::from_matches(&implicit).source("max_attempts"),
            Some(ArgumentSource::Default)
        );
        assert_eq!(
            CommandArgumentProvenance::from_matches(&explicit).source("max_attempts"),
            Some(ArgumentSource::CommandLine)
        );
    }

    #[test]
    fn captures_environment_and_allows_non_parser_resolution_sources() {
        let previous = std::env::var_os("HOMEBOY_PROVENANCE_TEST_ENV");
        std::env::set_var("HOMEBOY_PROVENANCE_TEST_ENV", "configured");
        let result = (|| {
            let matches = TestCli::command()
                .try_get_matches_from(["homeboy", "cook"])
                .expect("parse environment default");
            let mut provenance = CommandArgumentProvenance::from_matches(&matches);
            assert_eq!(
                provenance.source("from_env"),
                Some(ArgumentSource::Environment)
            );

            provenance.set("base", ArgumentSource::Configuration);
            provenance.set("head", ArgumentSource::Policy);
            provenance.set("run_id", ArgumentSource::Generated);
            let mut plan = serde_json::json!({"schema": "test/plan/v1"});
            provenance.project_into(&mut plan);
            assert_eq!(plan["command_argument_provenance"]["base"], "configuration");
            provenance
                .require_sources(
                    &["base", "head"],
                    &[ArgumentSource::Configuration, ArgumentSource::Policy],
                )
                .expect("allowed source group");
        })();
        if let Some(previous) = previous {
            std::env::set_var("HOMEBOY_PROVENANCE_TEST_ENV", previous);
        } else {
            std::env::remove_var("HOMEBOY_PROVENANCE_TEST_ENV");
        }
        result
    }

    #[test]
    fn rejects_disallowed_sources_before_effects() {
        let matches = TestCli::command()
            .try_get_matches_from(["homeboy", "cook", "--max-attempts", "3"])
            .expect("parse command");
        let provenance = CommandArgumentProvenance::from_matches(&matches);
        let error = provenance
            .require_sources(&["max_attempts"], &[ArgumentSource::Policy])
            .expect_err("explicit override must be rejected");

        assert_eq!(error.rejected[0].argument, "max_attempts");
        assert_eq!(error.rejected[0].source, ArgumentSource::CommandLine);
    }

    #[test]
    fn tracker_cook_fixture_keeps_resolved_values_and_rejects_cli_policy_overrides() {
        // This is the #10889 handoff shape. It intentionally contains no
        // tracker implementation; that feature supplies these typed values.
        #[derive(Debug, PartialEq, Eq)]
        struct TrackerCookFixture {
            issue: u64,
            base: String,
            max_attempts: u32,
            placement: String,
        }

        let compiled = TrackerCookArgumentAdapter::compile(
            TrackerCookFixture {
                issue: 10889,
                base: "main".to_string(),
                max_attempts: 3,
                placement: "lab".to_string(),
            },
            [
                ("base", ArgumentSource::Configuration),
                ("max_attempts", ArgumentSource::Policy),
                ("placement", ArgumentSource::Policy),
            ],
        );
        TrackerCookArgumentAdapter::require_policy_owned(
            &compiled.provenance,
            &["base", "max_attempts", "placement"],
        )
        .expect("tracker policy sources are accepted");
        assert_eq!(compiled.value.issue, 10889);

        let mut overridden = compiled.provenance.clone();
        overridden.set("base", ArgumentSource::CommandLine);
        let error = TrackerCookArgumentAdapter::require_policy_owned(&overridden, &["base"])
            .expect_err("explicit base override is rejected before mutation");
        assert_eq!(error.rejected[0].source, ArgumentSource::CommandLine);
    }
}
