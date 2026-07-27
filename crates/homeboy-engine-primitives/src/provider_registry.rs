//! Declarative process-global provider registries.
//!
//! Homeboy inverts a lot of subsystem behavior behind "provider" hooks: a leaf
//! layer declares a trait plus a no-op implementation, an owning layer registers
//! a real implementation at startup, and the leaf layer dispatches through a
//! process-global slot. The registry half of that pattern — the `static`
//! `Mutex<Option<_>>` slot, the `register_*` entry point, and the accessor that
//! falls back to the no-op — is pure ceremony and was hand-copied across ~35
//! modules in five crates.
//!
//! The copies drifted. Roughly two thirds reached for
//! `.expect("... provider lock")`, which turns a poisoned registry lock into a
//! process abort; the rest used
//! `.unwrap_or_else(|poisoned| poisoned.into_inner())`, which recovers. The
//! choice tracked whichever file happened to be copied, not any deliberate
//! policy.
//!
//! These macros define that policy once: **a poisoned provider-registry lock
//! always recovers via `into_inner()`.** The registry slot holds a `Box`/`Arc`
//! trait object and nothing else; a panic elsewhere in the process cannot leave
//! it in a torn state, so refusing to hand it out — and taking the process down
//! with it — is strictly worse than handing back the provider that is still
//! perfectly valid.
//!
//! What the macros deliberately do **not** own: the trait itself, the no-op
//! implementation, and the per-method dispatch functions. Those carry the real
//! per-site meaning (and their doc comments), so they stay hand-written and the
//! macro stays a two-form, no-knob tool.
//!
//! # Forms
//!
//! `provider_registry!` — boxed provider with a no-op fallback. Generates a
//! private slot accessor, the registration entry point, and a `with_*` accessor
//! that runs a closure against the registered provider or the no-op:
//!
//! ```ignore
//! homeboy_engine_primitives::provider_registry! {
//!     provider: dyn StackProvider,
//!     noop: NoopProvider,
//!     /// Register the stack provider. Called once at startup by the stack layer.
//!     register: pub fn register_stack_provider,
//!     with: fn with_provider,
//! }
//!
//! pub(crate) fn stack_list_json() -> Result<Value> {
//!     with_provider(|p| p.stack_list_json())
//! }
//! ```
//!
//! `provider_registry!` with `with_optional:` — boxed provider with **no** no-op
//! implementation, for registries whose dispatch functions each return their own
//! "subsystem absent" error. The accessor yields `Option<&dyn Trait>`.
//!
//! `provider_registry_arc!` — `Arc` provider whose accessor clones the `Arc`
//! out, so the registry lock is released before a (potentially long-running)
//! call. Used where holding the lock across the call would serialize real work.

/// Declare a process-global boxed provider registry.
///
/// See the [module docs](self) for the full rationale. Two forms:
///
/// - `noop: <expr>` + `with:` — accessor runs the closure against the
///   registered provider or the no-op.
/// - `with_optional:` (no `noop:`) — accessor runs the closure against
///   `Option<&dyn Trait>` so each dispatch function supplies its own fallback.
///
/// A poisoned lock always recovers via `into_inner()`.
#[macro_export]
macro_rules! provider_registry {
    (
        provider: dyn $provider:path,
        noop: $noop:expr,
        $(#[$register_meta:meta])*
        register: $register_vis:vis fn $register:ident,
        $(#[$with_meta:meta])*
        with: $with_vis:vis fn $with:ident $(,)?
    ) => {
        $crate::__provider_registry_slot!($provider);

        $(#[$register_meta])*
        $register_vis fn $register(provider: ::std::boxed::Box<dyn $provider>) {
            let mut slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = ::std::option::Option::Some(provider);
        }

        $(#[$with_meta])*
        $with_vis fn $with<__ProviderRegistryOut>(
            f: impl ::std::ops::FnOnce(&dyn $provider) -> __ProviderRegistryOut,
        ) -> __ProviderRegistryOut {
            let slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match slot.as_deref() {
                ::std::option::Option::Some(provider) => f(provider),
                ::std::option::Option::None => f(&$noop),
            }
        }
    };

    (
        provider: dyn $provider:path,
        $(#[$register_meta:meta])*
        register: $register_vis:vis fn $register:ident,
        $(#[$with_meta:meta])*
        with_optional: $with_vis:vis fn $with:ident $(,)?
    ) => {
        $crate::__provider_registry_slot!($provider);

        $(#[$register_meta])*
        $register_vis fn $register(provider: ::std::boxed::Box<dyn $provider>) {
            let mut slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = ::std::option::Option::Some(provider);
        }

        $(#[$with_meta])*
        $with_vis fn $with<__ProviderRegistryOut>(
            f: impl ::std::ops::FnOnce(
                ::std::option::Option<&dyn $provider>,
            ) -> __ProviderRegistryOut,
        ) -> __ProviderRegistryOut {
            let slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            f(slot.as_deref())
        }
    };
}

/// Declare a process-global `Arc` provider registry whose accessor clones the
/// `Arc` out before returning, releasing the registry lock so it is not held
/// across the provider call.
///
/// A poisoned lock always recovers via `into_inner()`.
#[macro_export]
macro_rules! provider_registry_arc {
    (
        provider: dyn $provider:path,
        noop: $noop:expr,
        $(#[$register_meta:meta])*
        register: $register_vis:vis fn $register:ident,
        $(#[$active_meta:meta])*
        active: $active_vis:vis fn $active:ident $(,)?
    ) => {
        fn provider_slot() -> &'static ::std::sync::Mutex<
            ::std::option::Option<::std::sync::Arc<dyn $provider>>,
        > {
            static PROVIDER: ::std::sync::Mutex<
                ::std::option::Option<::std::sync::Arc<dyn $provider>>,
            > = ::std::sync::Mutex::new(::std::option::Option::None);
            &PROVIDER
        }

        $(#[$register_meta])*
        $register_vis fn $register(provider: ::std::sync::Arc<dyn $provider>) {
            let mut slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = ::std::option::Option::Some(provider);
        }

        $(#[$active_meta])*
        $active_vis fn $active() -> ::std::sync::Arc<dyn $provider> {
            let slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match slot.as_ref() {
                ::std::option::Option::Some(provider) => ::std::sync::Arc::clone(provider),
                ::std::option::Option::None => ::std::sync::Arc::new($noop),
            }
        }
    };
}

/// Internal: the boxed slot accessor shared by both `provider_registry!` forms.
/// The `static` lives inside the function body so it cannot collide with any
/// module-level item at the expansion site.
#[doc(hidden)]
#[macro_export]
macro_rules! __provider_registry_slot {
    ($provider:path) => {
        fn provider_slot(
        ) -> &'static ::std::sync::Mutex<::std::option::Option<::std::boxed::Box<dyn $provider>>> {
            static PROVIDER: ::std::sync::Mutex<
                ::std::option::Option<::std::boxed::Box<dyn $provider>>,
            > = ::std::sync::Mutex::new(::std::option::Option::None);
            &PROVIDER
        }
    };
}

#[cfg(test)]
mod tests {
    trait Greeter: Send + Sync {
        fn greet(&self) -> String;
    }

    struct NoopGreeter;

    impl Greeter for NoopGreeter {
        fn greet(&self) -> String {
            "noop".to_string()
        }
    }

    struct RealGreeter;

    impl Greeter for RealGreeter {
        fn greet(&self) -> String {
            "real".to_string()
        }
    }

    crate::provider_registry! {
        provider: dyn Greeter,
        noop: NoopGreeter,
        /// Register the greeter used by this test module.
        register: pub fn register_greeter,
        with: fn with_greeter,
    }

    // One test: the registry is a process global, so splitting these would make
    // them order-dependent on each other.
    #[test]
    fn falls_back_to_noop_registers_and_survives_a_poisoned_lock() {
        assert_eq!(with_greeter(|g| g.greet()), "noop");

        register_greeter(Box::new(RealGreeter));
        assert_eq!(with_greeter(|g| g.greet()), "real");

        // Poison the slot from a panicking thread, then prove the accessor
        // still hands the provider back instead of taking the process down.
        let _ = std::thread::spawn(|| {
            let _guard = provider_slot().lock().unwrap();
            panic!("poison the registry lock");
        })
        .join();
        assert!(provider_slot().is_poisoned());
        assert_eq!(with_greeter(|g| g.greet()), "real");
    }
}
