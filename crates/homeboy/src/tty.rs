use std::io;
use std::io::IsTerminal;

pub fn is_stdin_tty() -> bool {
    io::stdin().is_terminal()
}

pub fn is_stdout_tty() -> bool {
    io::stdout().is_terminal()
}

pub fn is_stderr_tty() -> bool {
    io::stderr().is_terminal()
}

pub fn require_tty_for_interactive() -> bool {
    is_stdin_tty() && is_stdout_tty()
}
