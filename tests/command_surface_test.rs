use homeboy::cli_surface::current_command_surface;

#[test]
fn includes_current_top_level_commands() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["audit"]));
    assert!(surface.contains_path(&["daemon"]));
    assert!(surface.contains_path(&["git"]));
    assert!(surface.contains_path(&["stack"]));
}

#[test]
fn includes_first_level_subcommands() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["git", "status"]));
    assert!(surface.contains_path(&["daemon", "serve"]));
    assert!(surface.contains_path(&["stack", "inspect"]));
}

#[test]
fn includes_visible_aliases() {
    let surface = current_command_surface();

    assert!(surface.contains_path(&["components"]));
    assert!(surface.contains_path(&["rigs"]));
    assert!(surface.contains_path(&["stacks", "inspect"]));
}

#[test]
fn rejects_stale_or_deeper_paths() {
    let surface = current_command_surface();

    assert!(!surface.contains_path(&["supports"]));
    assert!(!surface.contains_path(&["audit", "code"]));
    assert!(!surface.contains_path(&["stack", "inspect", "extra"]));
}
