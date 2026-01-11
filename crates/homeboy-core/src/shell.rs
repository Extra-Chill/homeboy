use crate::{Error, Result};

pub fn escape_single_quotes(input: &str) -> String {
    input.replace('"', "\\\"").replace('\\', "\\\\")
}

pub fn escape_shell_single_quoted(input: &str) -> String {
    input.replace('"', "\\\"")
}

pub fn cd_and(dir: &str, command: &str) -> Result<String> {
    let dir = dir.trim();
    let command = command.trim();

    if dir.is_empty() {
        return Err(Error::Config("Directory cannot be empty".to_string()));
    }

    if command.is_empty() {
        return Err(Error::Config("Command cannot be empty".to_string()));
    }

    Ok(format!(
        "cd '{}' && {}",
        dir.replace('\'', "'\\''"),
        command
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_and_wraps_command() {
        assert_eq!(
            cd_and("/var/www", "wp option get blogname").unwrap(),
            "cd '/var/www' && wp option get blogname"
        );
    }

    #[test]
    fn cd_and_escapes_single_quotes() {
        assert_eq!(
            cd_and("/var/www/it's", "echo ok").unwrap(),
            "cd '/var/www/it'\\''s' && echo ok"
        );
    }
}
