use super::*;
use homeboy_core::test_support::with_isolated_home;

fn fixture(root: &Path) -> PathBuf {
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let candidate = root.join("candidate");
    homeboy_test_support::write_executable(
        &candidate,
        &format!("#!/bin/sh\ncase \"$1\" in\n--version) echo 'homeboy 99.0.0' ;;\n--observation-schema-version) echo {} ;;\nself) echo '{{\"version\":\"99.0.0\"}}' ;;\nesac\n", homeboy_core::observation::store::CURRENT_SCHEMA_VERSION),
        0o755,
    ).unwrap();
    // Intercept only the canonical installer's stage, not the slot script.
    homeboy_test_support::write_executable(
        &tools.join("sh"),
        &format!("#!/bin/sh\nif [ \"$HOMEBOY_INSTALL_STAGE_ONLY\" = 1 ]; then\n echo download >> '{}'\n cp '{}' \"$HOMEBOY_INSTALL_PATH\"\n exit\nfi\nexec /bin/sh \"$@\"\n", root.join("downloads").display(), candidate.display()),
        0o755,
    ).unwrap();
    std::env::set_var(
        "PATH",
        format!("{}:{}", tools.display(), std::env::var("PATH").unwrap()),
    );
    candidate
}

#[test]
fn selected_release_upgrade_downloads_once_cold_and_zero_when_prefetched() {
    for preacquired in [false, true] {
        with_isolated_home(|home| {
            let candidate = fixture(home.path());
            let tools = home.path().join("tools");
            let live = tools.join("homeboy");
            homeboy_test_support::write_executable(
                &live,
                "#!/bin/sh\necho 'homeboy 0.1.0'\n",
                0o755,
            )
            .unwrap();
            let binaries = binaries_dir().unwrap();
            if preacquired {
                acquire_release_slot("v99.0.0", &binaries).unwrap();
            }
            let downloads = home.path().join("downloads");
            let before = std::fs::read_to_string(&downloads)
                .unwrap_or_default()
                .lines()
                .count();
            let lease =
                homeboy_core::runtime_promotion::acquire("upgrade fixture", "controller").unwrap();
            let release = crate::SelectedRelease {
                tag: "v99.0.0".into(),
                version: "99.0.0".into(),
                target: None,
            };
            let result = super::super::execution::execute_upgrade(
                crate::InstallMethod::Binary,
                None,
                false,
                false,
                false,
                None,
                Some(&release),
                Some(&lease),
                &mut |_| Ok(()),
                &mut |_| Ok(()),
            )
            .unwrap();
            assert!(result.0);
            assert_eq!(
                std::fs::read(&live).unwrap(),
                std::fs::read(candidate).unwrap()
            );
            let after = std::fs::read_to_string(downloads).unwrap().lines().count();
            assert_eq!(after - before, usize::from(!preacquired));
        });
    }
}

#[test]
fn prefetch_second_pass_hits_slot_and_prunes_old_slots() {
    with_isolated_home(|home| {
        fixture(home.path());
        std::env::remove_var("HOMEBOY_NO_UPDATE_CHECK");
        let cache = homeboy_core::update_check_cache::cache_path("update_check.json").unwrap();
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(
            cache,
            serde_json::to_vec(&super::super::update_check::UpdateCheckCache {
                latest_version: "99.0.0".into(),
                current_version: crate::current_version().into(),
                update_available: true,
                checked_at: homeboy_core::update_check_cache::now_unix(),
                runtime_compatibility: None,
            })
            .unwrap(),
        )
        .unwrap();
        let binaries = binaries_dir().unwrap();
        let old = binaries.join(format!("homeboy-{}", "a".repeat(64)));
        std::fs::create_dir_all(&old).unwrap();
        let running_sha = "b".repeat(64);
        let running = binaries.join(format!("homeboy-{running_sha}"));
        std::fs::create_dir_all(&running).unwrap();
        std::fs::write(
            binaries.join(format!(".release-v{}.sha256", crate::current_version())),
            running_sha,
        )
        .unwrap();
        let first = crate::update_check::prefetch_release().unwrap().unwrap();
        assert!(!old.exists());
        assert!(running.exists(), "pruning preserves the running release");
        assert_eq!(
            crate::update_check::prefetch_release().unwrap(),
            Some(first)
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join("downloads")).unwrap(),
            "download\n"
        );
        std::env::set_var("HOMEBOY_NO_UPDATE_CHECK", "1");
        assert_eq!(crate::update_check::prefetch_release().unwrap(), None);
    });
}
