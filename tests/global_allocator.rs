use std::process::Command;

#[test]
fn shipped_binary_uses_the_global_allocator() {
    let output = Command::new(env!("CARGO_BIN_EXE_homeboy"))
        .arg("--version")
        .env("MIMALLOC_VERBOSE", "1")
        .output()
        .expect("run the shipped homeboy binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "homeboy --version failed: {stderr}"
    );
    assert!(
        stderr.contains("mimalloc: "),
        "missing mimalloc startup diagnostics in stderr: {stderr}"
    );
}
