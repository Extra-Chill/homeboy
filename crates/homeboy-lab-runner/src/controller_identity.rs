//! The controller build identity that runner compatibility is judged against.
//!
//! Normally that is this process's own build. `homeboy upgrade` is the
//! exception: it installs a new controller and then converges configured
//! runners from the *pre-upgrade* process. Judging those runners against the
//! running (old) build reports every runner that converged to the installed
//! controller as incompatible, so the upgrade declares "require repair" for a
//! runner that is exactly where it should be (#15552).
//!
//! [`with_converging_controller`] scopes the installed controller identity
//! over the upgrade's runner phase. The override is thread-local, so it never
//! leaks into concurrent work or other tests, and it only changes what runner
//! compatibility (and the recovery it recommends) is compared to — it never
//! changes what this process reports as its own identity.

use homeboy_product_identity::BuildIdentity;
use std::cell::RefCell;

thread_local! {
    static CONVERGING_CONTROLLER: RefCell<Option<BuildIdentity>> = const { RefCell::new(None) };
}

/// The controller identity runners must be compatible with: the scoped
/// converging controller when one is set, else this process's build.
pub(crate) fn compatibility_controller_identity() -> BuildIdentity {
    CONVERGING_CONTROLLER
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(homeboy_product_identity::build_identity)
}

/// Run `f` with runner compatibility judged against `identity` (a
/// `homeboy <version>+<commit>` display). An unparseable or absent identity
/// leaves the process's own build in effect.
pub(crate) fn with_converging_controller<T>(identity: Option<&str>, f: impl FnOnce() -> T) -> T {
    let Some(identity) = identity.and_then(homeboy_upgrade::upgrade::parse_build_identity_display)
    else {
        return f();
    };
    let previous = CONVERGING_CONTROLLER.with(|slot| slot.replace(Some(identity)));
    struct Restore(Option<BuildIdentity>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            CONVERGING_CONTROLLER.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let _restore = Restore(previous);
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_this_process_build() {
        assert_eq!(
            compatibility_controller_identity(),
            homeboy_product_identity::build_identity()
        );
    }

    #[test]
    fn scopes_the_converging_controller_and_restores_after() {
        let installed = "homeboy 99.0.0+dd97ab6dd5395907573bd4525f60f67e1e38af56";
        let seen = with_converging_controller(Some(installed), compatibility_controller_identity);
        assert_eq!(seen.display, installed);
        assert_eq!(seen.version, "99.0.0");
        assert_eq!(
            seen.git_commit.as_deref(),
            Some("dd97ab6dd5395907573bd4525f60f67e1e38af56")
        );
        assert_eq!(
            compatibility_controller_identity(),
            homeboy_product_identity::build_identity()
        );
    }

    #[test]
    fn restores_after_a_panic() {
        let result = std::panic::catch_unwind(|| {
            with_converging_controller(Some("homeboy 99.0.0+abcdef1"), || panic!("boom"))
        });
        assert!(result.is_err());
        assert_eq!(
            compatibility_controller_identity(),
            homeboy_product_identity::build_identity()
        );
    }

    #[test]
    fn unparseable_identity_keeps_this_process_build() {
        let seen = with_converging_controller(Some("not an identity"), || {
            compatibility_controller_identity()
        });
        assert_eq!(seen, homeboy_product_identity::build_identity());
    }
}
