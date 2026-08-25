use std::collections::{BTreeMap, BTreeSet};

use clap::Command;

use crate::cli_runtime::CliCapability;

pub struct RegisteredCliCapability {
    pub capability: &'static dyn CliCapability,
    pub command: Command,
    spellings: Vec<String>,
}

pub struct CommandCapabilityRegistry {
    entries: Vec<RegisteredCliCapability>,
}

impl CommandCapabilityRegistry {
    pub fn compose(
        capabilities: &'static [&'static dyn CliCapability],
        required: &[&str],
        builtin: &Command,
    ) -> crate::core::Result<Self> {
        let mut builtin_owners = BTreeMap::new();
        for command in builtin.get_subcommands() {
            builtin_owners.insert(command.get_name().to_string(), "built-in command");
            for alias in command.get_all_aliases() {
                builtin_owners.insert(alias.to_string(), "built-in command alias");
            }
        }

        let mut owners = BTreeSet::new();
        let mut spellings = builtin_owners;
        let mut entries = Vec::with_capacity(capabilities.len());
        for capability in capabilities {
            let owner = capability.name();
            if !owners.insert(owner) {
                return Err(registry_error(format!(
                    "capability owner `{owner}` is registered more than once"
                )));
            }
            let command = capability.command();
            if command.get_name() != owner {
                return Err(registry_error(format!(
                    "capability owner `{owner}` returned command `{}`",
                    command.get_name()
                )));
            }
            let mut entry_spellings = vec![owner.to_string()];
            entry_spellings.extend(command.get_all_aliases().map(str::to_string));
            entry_spellings.sort();
            entry_spellings.dedup();
            for spelling in &entry_spellings {
                if let Some(existing) = spellings.insert(spelling.clone(), owner) {
                    return Err(registry_error(format!(
                        "capability `{owner}` spelling `{spelling}` conflicts with {existing}"
                    )));
                }
            }
            entries.push(RegisteredCliCapability {
                capability: *capability,
                command,
                spellings: entry_spellings,
            });
        }

        for owner in required {
            if !owners.contains(owner) {
                return Err(registry_error(format!(
                    "required capability owner `{owner}` is not registered"
                )));
            }
        }
        entries.sort_by_key(|entry| entry.capability.name());
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[RegisteredCliCapability] {
        &self.entries
    }

    pub fn capabilities(&self) -> Vec<&'static dyn CliCapability> {
        self.entries.iter().map(|entry| entry.capability).collect()
    }

    pub fn find(&self, spelling: &str) -> Option<&'static dyn CliCapability> {
        self.entries
            .iter()
            .find(|entry| entry.spellings.iter().any(|value| value == spelling))
            .map(|entry| entry.capability)
    }

    pub fn validate_external_names<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> crate::core::Result<()> {
        for name in names {
            if let Some(owner) = self.find(name) {
                return Err(registry_error(format!(
                    "capability `{}` spelling `{name}` conflicts with dynamic command `{name}`",
                    owner.name()
                )));
            }
        }
        Ok(())
    }
}

fn registry_error(problem: String) -> crate::core::Error {
    crate::core::Error::validation_invalid_argument("capabilities", problem, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ArgMatches;

    struct Alpha;
    struct Beta;
    struct AlphaAliasBeta;
    struct Mismatched;
    struct StatusCapability;

    static ALPHA: Alpha = Alpha;
    static BETA: Beta = Beta;
    static ALPHA_ALIAS_BETA: AlphaAliasBeta = AlphaAliasBeta;
    static MISMATCHED: Mismatched = Mismatched;
    static STATUS_CAPABILITY: StatusCapability = StatusCapability;

    macro_rules! capability {
        ($type:ty, $name:literal, $command:expr) => {
            impl CliCapability for $type {
                fn name(&self) -> &'static str {
                    $name
                }

                fn command(&self) -> Command {
                    $command
                }

                fn run(
                    &self,
                    _matches: &ArgMatches,
                ) -> crate::core::Result<(serde_json::Value, i32)> {
                    Ok((serde_json::Value::Null, 0))
                }
            }
        };
    }

    capability!(Alpha, "alpha", Command::new("alpha"));
    capability!(Beta, "beta", Command::new("beta"));
    capability!(AlphaAliasBeta, "alpha", Command::new("alpha").alias("beta"));
    capability!(Mismatched, "declared", Command::new("actual"));
    capability!(StatusCapability, "status", Command::new("status"));

    fn builtin() -> Command {
        Command::new("homeboy").subcommand(Command::new("status"))
    }

    fn error(
        capabilities: &'static [&'static dyn CliCapability],
        required: &[&str],
    ) -> crate::core::Error {
        CommandCapabilityRegistry::compose(capabilities, required, &builtin())
            .err()
            .expect("composition must fail")
    }

    #[test]
    fn registry_order_is_stable_under_permuted_input() {
        static FORWARD: [&'static dyn CliCapability; 2] = [&ALPHA, &BETA];
        static REVERSE: [&'static dyn CliCapability; 2] = [&BETA, &ALPHA];
        let names = |registry: CommandCapabilityRegistry| {
            registry
                .entries()
                .iter()
                .map(|entry| entry.capability.name())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(CommandCapabilityRegistry::compose(&FORWARD, &[], &builtin()).unwrap()),
            names(CommandCapabilityRegistry::compose(&REVERSE, &[], &builtin()).unwrap())
        );
    }

    #[test]
    fn registry_rejects_conflicts_and_missing_required_owners() {
        static ALIAS_CONFLICT: [&'static dyn CliCapability; 2] = [&ALPHA_ALIAS_BETA, &BETA];
        static DUPLICATE: [&'static dyn CliCapability; 2] = [&ALPHA, &ALPHA];
        static BUILTIN_CONFLICT: [&'static dyn CliCapability; 1] = [&STATUS_CAPABILITY];
        static MISMATCH: [&'static dyn CliCapability; 1] = [&MISMATCHED];
        static ONLY_ALPHA: [&'static dyn CliCapability; 1] = [&ALPHA];

        for error in [
            error(&ALIAS_CONFLICT, &[]),
            error(&DUPLICATE, &[]),
            error(&BUILTIN_CONFLICT, &[]),
            error(&MISMATCH, &[]),
            error(&ONLY_ALPHA, &["beta"]),
        ] {
            assert_eq!(error.details["field"], "capabilities");
        }
    }

    #[test]
    fn empty_kernel_registry_is_valid() {
        let registry = CommandCapabilityRegistry::compose(&[], &[], &builtin()).unwrap();
        assert!(registry.entries().is_empty());
    }
}
