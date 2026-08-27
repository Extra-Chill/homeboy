/// Filesystem-safe output stem for a command request.
pub(crate) fn command_output_stem(command: &str) -> String {
    let mut stem = String::with_capacity(command.len());
    let mut last_was_separator = false;
    for character in command.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            stem.push(character);
            last_was_separator = false;
        } else if !last_was_separator {
            stem.push('-');
            last_was_separator = true;
        }
    }
    let stem = stem.trim_matches('-');
    if stem.is_empty() {
        "homeboy-output".to_string()
    } else {
        stem.to_string()
    }
}
