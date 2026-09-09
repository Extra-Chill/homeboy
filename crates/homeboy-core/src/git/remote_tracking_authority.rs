use crate::error::Result;
use std::path::Path;
use std::time::{Duration, Instant};

/// Serialize Homeboy operations that can update remote-tracking refs for one
/// repository. Linked worktrees share a Git common directory, while unrelated
/// repositories retain independent concurrency.
pub fn with_remote_tracking_authority_until<T>(
    repository: &Path,
    operation: &str,
    deadline: Instant,
    action: impl FnOnce(Duration) -> Result<T>,
) -> Result<T> {
    homeboy_engine_primitives::git_remote_tracking_authority::with_remote_tracking_authority_until(
        repository, operation, deadline, action,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;

    fn git(path: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git")
            .status
            .success());
    }

    fn repository(path: &Path) {
        git(path, &["init", "-q"]);
        git(path, &["config", "user.email", "homeboy@example.test"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(path.join("README"), "fixture\n").expect("write fixture");
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "fixture"]);
    }

    #[test]
    fn two_process_sibling_worktrees_serialize_fetch_paths() {
        const CHILD: &str = "HOMEB0Y_REMOTE_TRACKING_FETCH_CHILD";
        if let Some(path) = std::env::var_os(CHILD) {
            let path = Path::new(&path);
            match std::env::var("HOMEB0Y_REMOTE_TRACKING_FETCH_OPERATION").as_deref() {
                Ok("behind") => {
                    crate::git::fetch_and_get_behind_count(path.to_str().expect("path"))
                        .expect("child behind fetch");
                }
                Ok("origin") => {
                    crate::git::fetch_origin(path.to_str().expect("path"))
                        .expect("child origin fetch");
                }
                Ok("tags") => {
                    crate::git::fetch_tags(path.to_str().expect("path")).expect("child tag fetch");
                }
                operation => panic!("unexpected child fetch operation: {operation:?}"),
            }
            if let Some(done) = std::env::var_os("HOMEB0Y_REMOTE_TRACKING_FETCH_DONE") {
                std::fs::write(done, "done\n").expect("write child completion");
            }
            return;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        git(
            temp.path(),
            &["init", "--bare", "-q", remote.to_str().expect("path")],
        );
        let source = temp.path().join("source");
        std::fs::create_dir(&source).expect("source");
        repository(&source);
        git(
            &source,
            &["remote", "add", "origin", remote.to_str().expect("path")],
        );
        git(&source, &["push", "-q", "origin", "HEAD:main"]);
        let sibling = temp.path().join("sibling");
        git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                sibling.to_str().expect("path"),
            ],
        );

        let executable = std::env::current_exe().expect("test executable");
        let test_name =
            "git::remote_tracking_authority::tests::two_process_sibling_worktrees_serialize_fetch_paths";
        let wrapper_dir = temp.path().join("bin");
        std::fs::create_dir(&wrapper_dir).expect("wrapper directory");
        let started = temp.path().join("first-fetch-started");
        let release = temp.path().join("release-first-fetch");
        let upload_pack = wrapper_dir.join("git-upload-pack");
        std::fs::write(
            &upload_pack,
            "#!/bin/sh\nif [ -n \"$HOMEB0Y_REMOTE_TRACKING_FETCH_REACHED\" ]; then\n  touch \"$HOMEB0Y_REMOTE_TRACKING_FETCH_REACHED\"\nfi\nif [ -n \"$HOMEB0Y_REMOTE_TRACKING_FETCH_STARTED\" ]; then\n  touch \"$HOMEB0Y_REMOTE_TRACKING_FETCH_STARTED\"\n  while ! test -e \"$HOMEB0Y_REMOTE_TRACKING_FETCH_RELEASE\"; do sleep 0.01; done\nfi\nexec \"$HOMEB0Y_REMOTE_TRACKING_REAL_GIT\" upload-pack \"$@\"\n",
        )
        .expect("write upload-pack wrapper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&upload_pack, std::fs::Permissions::from_mode(0o755))
                .expect("make upload-pack wrapper executable");
        }
        git(
            &source,
            &[
                "config",
                "remote.origin.uploadpack",
                upload_pack.to_str().expect("upload-pack path"),
            ],
        );
        let real_git = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
            .map(|directory| directory.join("git"))
            .find_map(|candidate| candidate.canonicalize().ok())
            .expect("real git executable");
        let path_env = format!(
            "{}:{}",
            wrapper_dir.display(),
            std::env::var("PATH").expect("PATH")
        );
        let spawn = |path: &Path,
                     operation: &str,
                     hold_fetch: bool,
                     attempted: Option<&Path>,
                     reached: Option<&Path>,
                     done: Option<&Path>| {
            let mut command = Command::new(&executable);
            command
                .args(["--exact", test_name, "--nocapture"])
                .env(CHILD, path)
                .env("HOMEB0Y_REMOTE_TRACKING_FETCH_OPERATION", operation)
                .env("PATH", &path_env)
                .env("HOMEB0Y_REMOTE_TRACKING_REAL_GIT", &real_git);
            if hold_fetch {
                command
                    .env("HOMEB0Y_REMOTE_TRACKING_FETCH_STARTED", &started)
                    .env("HOMEB0Y_REMOTE_TRACKING_FETCH_RELEASE", &release);
            }
            if let Some(attempted) = attempted {
                command.env("HOMEB0Y_REMOTE_TRACKING_FETCH_LOCK_ATTEMPTED", attempted);
            }
            if let Some(reached) = reached {
                command.env("HOMEB0Y_REMOTE_TRACKING_FETCH_REACHED", reached);
            }
            if let Some(done) = done {
                command.env("HOMEB0Y_REMOTE_TRACKING_FETCH_DONE", done);
            }
            command.spawn().expect("spawn fetch child")
        };
        let mut first = spawn(&source, "behind", true, None, None, None);
        let wait_deadline = Instant::now() + Duration::from_secs(2);
        while !started.exists() && Instant::now() < wait_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started.exists(), "first fetch did not reach upload-pack");
        let second_attempted = temp.path().join("second-fetch-attempted");
        let second_reached = temp.path().join("second-fetch-reached-upload-pack");
        let second_done = temp.path().join("second-fetch-done");
        let mut second = spawn(
            &sibling,
            "origin",
            false,
            Some(&second_attempted),
            Some(&second_reached),
            Some(&second_done),
        );
        let attempt_deadline = Instant::now() + Duration::from_secs(2);
        while !second_attempted.exists() && Instant::now() < attempt_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            second_attempted.exists(),
            "second fetch did not contend for remote-tracking authority"
        );
        let contention_deadline = Instant::now() + Duration::from_millis(500);
        while !second_reached.exists() && Instant::now() < contention_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !second_reached.exists(),
            "sibling fetch reached transport while the first fetch held remote-tracking authority"
        );
        std::fs::write(&release, "release\n").expect("release first fetch");
        assert!(first.wait().expect("wait first").success());
        assert!(second.wait().expect("wait second").success());
        assert!(second_done.exists(), "sibling fetch did not complete");
        let mut tags = spawn(&sibling, "tags", false, None, None, None);
        assert!(tags.wait().expect("wait tag fetch").success());
    }

    #[test]
    fn sibling_worktrees_serialize_remote_tracking_operations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        std::fs::create_dir(&source).expect("source");
        repository(&source);
        let sibling = temp.path().join("sibling");
        git(
            &source,
            &[
                "worktree",
                "add",
                "--detach",
                sibling.to_str().expect("path"),
            ],
        );

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let source_for_thread = source.clone();
        let first = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &source_for_thread,
                "first fetch",
                Instant::now() + Duration::from_secs(5),
                |_| {
                    entered_tx.send(()).expect("entered");
                    release_rx.recv().expect("release");
                    git(
                        &source_for_thread,
                        &["update-ref", "refs/remotes/origin/concurrent", "HEAD"],
                    );
                    Ok(())
                },
            )
        });
        entered_rx.recv().expect("first entered");

        let (second_tx, second_rx) = mpsc::channel();
        let second = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &sibling,
                "second fetch",
                Instant::now() + Duration::from_secs(5),
                |_| {
                    git(
                        &sibling,
                        &["update-ref", "refs/remotes/origin/concurrent", "HEAD"],
                    );
                    second_tx.send(()).expect("second entered");
                    Ok(())
                },
            )
        });
        assert!(second_rx.recv_timeout(Duration::from_millis(150)).is_err());
        release_tx.send(()).expect("release first");
        first
            .join()
            .expect("first thread")
            .expect("first authority");
        second_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second entered after release");
        second
            .join()
            .expect("second thread")
            .expect("second authority");
        git(
            &source,
            &["rev-parse", "--verify", "refs/remotes/origin/concurrent"],
        );
    }

    #[test]
    fn unrelated_repositories_keep_remote_tracking_concurrency() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_repo = temp.path().join("first");
        let second_repo = temp.path().join("second");
        std::fs::create_dir(&first_repo).expect("first repo");
        std::fs::create_dir(&second_repo).expect("second repo");
        repository(&first_repo);
        repository(&second_repo);

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = thread::spawn(move || {
            with_remote_tracking_authority_until(
                &first_repo,
                "first fetch",
                Instant::now() + Duration::from_secs(5),
                |_| {
                    entered_tx.send(()).expect("first entered");
                    release_rx.recv().expect("release");
                    Ok(())
                },
            )
        });
        entered_rx.recv().expect("first entered");
        with_remote_tracking_authority_until(
            &second_repo,
            "second fetch",
            Instant::now() + Duration::from_secs(5),
            |_| Ok(()),
        )
        .expect("unrelated authority");
        release_tx.send(()).expect("release first");
        first
            .join()
            .expect("first thread")
            .expect("first authority");
    }
}
