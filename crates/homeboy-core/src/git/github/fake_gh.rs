//! Test-only fake-`gh` fixture shared by the REST mutation argv tests in
//! [`super::pulls`] and [`super::issues`].
//!
//! Registers a component whose `remote_url` points at `github.com/example/<id>`
//! and puts a caller-supplied `gh` shell script first on `PATH`. The script is
//! expected to append `"$*"` to `$HOMEBOY_FAKE_GH_LOG` so tests can assert the
//! exact argv each mutation produced.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::test_support::EnvVarGuard;

pub(super) struct FakeGhFixture {
    pub(super) log: PathBuf,
    _path_env: EnvVarGuard,
    _log_env: EnvVarGuard,
}

pub(super) fn fake_gh_component(home: &Path, id: &str, gh_script: &str) -> FakeGhFixture {
    let source = home.join(format!("{id}-checkout"));
    std::fs::create_dir_all(&source).expect("component checkout directory");
    let registrations = home.join(".config/homeboy/components");
    std::fs::create_dir_all(&registrations).expect("component registrations directory");
    std::fs::write(
        registrations.join(format!("{id}.json")),
        serde_json::json!({
            "local_path": source,
            "remote_url": format!("https://github.com/example/{id}.git"),
        })
        .to_string(),
    )
    .expect("component registration");

    let bin = home.join("fake-gh-bin");
    std::fs::create_dir_all(&bin).expect("fake gh bin directory");
    let gh = bin.join("gh");
    std::fs::write(&gh, gh_script).expect("write fake gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755))
        .expect("make fake gh executable");

    let log = home.join("gh.log");
    let path_env = EnvVarGuard::set(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let log_env = EnvVarGuard::set("HOMEBOY_FAKE_GH_LOG", &log);

    FakeGhFixture {
        log,
        _path_env: path_env,
        _log_env: log_env,
    }
}

impl FakeGhFixture {
    /// The `gh api …` invocations recorded by the fake `gh`, one argv string
    /// per call, in order. Readiness probes (`--version`, `auth status`) are
    /// filtered out.
    pub(super) fn api_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .expect("fake gh log")
            .lines()
            .filter(|line| line.starts_with("api "))
            .map(ToString::to_string)
            .collect()
    }
}
