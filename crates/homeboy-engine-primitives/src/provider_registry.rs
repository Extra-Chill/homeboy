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
//! Its `optional:` form is the `Arc` counterpart of `with_optional:`: no no-op,
//! accessor yields `Option<Arc<dyn Trait>>`.
//!
//! # Declaration inventory
//!
//! Every expansion also submits a [`DeclaredProviderRegistry`] descriptor to a
//! link-time inventory. That closes the pattern's one genuinely silent failure
//! mode: an unregistered boxed registry dispatches to its no-op, so deleting a
//! `register_*()` call at startup produces no error, no log, and no test
//! failure — the subsystem just stops doing anything. Because the descriptors
//! are contributed by the declaration site rather than hand-listed, a registry
//! that nobody ever wired up is visible to
//! [`unregistered_provider_registry_ids`] the moment it exists.
//!
//! The inventory is pure metadata: it does not change what an unregistered
//! registry *does*, only whether anything can notice. `homeboy-cli`'s
//! `register_all_providers` completeness test is the consumer that turns
//! noticing into a failing build.

/// One declared provider registry, contributed to a link-time inventory by
/// every `provider_registry!` / `provider_registry_arc!` expansion in the
/// process.
///
/// The descriptor is metadata plus one probe. It exists so that "was this
/// registry ever wired up?" is an answerable question: without it, an
/// unregistered boxed registry is indistinguishable at runtime from one whose
/// provider happens to do nothing.
#[derive(Clone, Copy)]
pub struct DeclaredProviderRegistry {
    /// `module_path!()` of the declaration site. Unique per registry: two
    /// registries in one module would collide on the generated slot accessor,
    /// so a module hosts at most one.
    pub module_path: &'static str,
    /// The name of the generated registration entry point, e.g.
    /// `register_stack_provider`.
    pub register_fn: &'static str,
    /// Whether the slot currently holds a provider. Takes the registry lock,
    /// recovering from poisoning exactly like the accessors do.
    pub is_registered: fn() -> bool,
}

impl DeclaredProviderRegistry {
    /// Stable identifier: the declaration site's module path joined to the
    /// registration entry point's name.
    pub fn id(&self) -> String {
        format!("{}::{}", self.module_path, self.register_fn)
    }

    /// Whether a provider has been registered into this registry.
    pub fn registered(&self) -> bool {
        (self.is_registered)()
    }
}

impl std::fmt::Debug for DeclaredProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeclaredProviderRegistry")
            .field("id", &self.id())
            .finish()
    }
}

inventory::collect!(DeclaredProviderRegistry);

/// Every provider registry declared by any crate linked into the current
/// binary, sorted by [`DeclaredProviderRegistry::id`].
///
/// Only registries whose declaring crate is linked in are visible, which is
/// exactly the set that binary can be expected to register.
pub fn declared_provider_registries() -> Vec<&'static DeclaredProviderRegistry> {
    let mut declared = inventory::iter::<DeclaredProviderRegistry>
        .into_iter()
        .collect::<Vec<_>>();
    // Sorted by `id()` rather than by the `(module_path, register_fn)` tuple:
    // the two orders differ (a parent module sorts before its child under the
    // tuple, after it once `::` is part of the string), and `id()` is what
    // callers report and diff against.
    declared.sort_by_key(|registry| registry.id());
    declared
}

/// The ids of every declared registry whose slot is still empty.
///
/// An empty boxed registry silently dispatches to its no-op, so this is the
/// list of subsystems that are currently inert. A completeness test asserts it
/// contains nothing but the registries that are deliberately conditional.
pub fn unregistered_provider_registry_ids() -> Vec<String> {
    declared_provider_registries()
        .into_iter()
        .filter(|registry| !registry.registered())
        .map(DeclaredProviderRegistry::id)
        .collect()
}

/// Internal: submit the declaration-site descriptor for the registry whose
/// slot accessor was just generated. Shared by all four macro forms.
///
/// The probe closes over the enclosing module's generated `provider_slot()`,
/// so it reports the live state of that specific registry.
#[doc(hidden)]
#[macro_export]
macro_rules! __provider_registry_declare {
    ($register:ident) => {
        const _: () = {
            fn is_registered() -> bool {
                provider_slot()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_some()
            }

            $crate::inventory::submit! {
                $crate::provider_registry::DeclaredProviderRegistry {
                    module_path: ::std::module_path!(),
                    register_fn: ::std::stringify!($register),
                    is_registered: is_registered,
                }
            }
        };
    };
}

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
        $crate::__provider_registry_declare!($register);

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
        $crate::__provider_registry_declare!($register);

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
/// Two forms, mirroring [`provider_registry!`]:
///
/// - `noop: <expr>` + `active:` — the accessor always yields an `Arc`, falling
///   back to a freshly-allocated no-op.
/// - `optional:` (no `noop:`) — the accessor yields `Option<Arc<dyn Trait>>` so
///   each caller supplies its own "provider absent" behavior.
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
        $crate::__provider_registry_arc_slot!($provider);
        $crate::__provider_registry_declare!($register);

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

    (
        provider: dyn $provider:path,
        $(#[$register_meta:meta])*
        register: $register_vis:vis fn $register:ident,
        $(#[$active_meta:meta])*
        optional: $active_vis:vis fn $active:ident $(,)?
    ) => {
        $crate::__provider_registry_arc_slot!($provider);
        $crate::__provider_registry_declare!($register);

        $(#[$register_meta])*
        $register_vis fn $register(provider: ::std::sync::Arc<dyn $provider>) {
            let mut slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = ::std::option::Option::Some(provider);
        }

        $(#[$active_meta])*
        $active_vis fn $active(
        ) -> ::std::option::Option<::std::sync::Arc<dyn $provider>> {
            let slot = provider_slot()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slot.as_ref().map(::std::sync::Arc::clone)
        }
    };
}

/// Internal: the `Arc` slot accessor shared by both `provider_registry_arc!`
/// forms. The `static` lives inside the function body so it cannot collide with
/// any module-level item at the expansion site.
#[doc(hidden)]
#[macro_export]
macro_rules! __provider_registry_arc_slot {
    ($provider:path) => {
        fn provider_slot(
        ) -> &'static ::std::sync::Mutex<::std::option::Option<::std::sync::Arc<dyn $provider>>> {
            static PROVIDER: ::std::sync::Mutex<
                ::std::option::Option<::std::sync::Arc<dyn $provider>>,
            > = ::std::sync::Mutex::new(::std::option::Option::None);
            &PROVIDER
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
    pub(crate) trait Greeter: Send + Sync {
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
        register: pub(crate) fn register_greeter,
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

    // A second registry in the same module would collide on the generated
    // `provider_slot`, so the `Arc` forms get their own modules.
    mod arc_optional {
        use super::Greeter;
        use std::sync::Arc;

        struct RealGreeter;

        impl Greeter for RealGreeter {
            fn greet(&self) -> String {
                "real".to_string()
            }
        }

        crate::provider_registry_arc! {
            provider: dyn Greeter,
            /// Register the greeter used by this test module.
            register: pub(crate) fn register_greeter,
            /// The registered greeter, if any.
            optional: fn active_greeter,
        }

        // One test: the registry is a process global, so splitting these would
        // make them order-dependent on each other.
        #[test]
        fn yields_none_registers_and_survives_a_poisoned_lock() {
            assert!(active_greeter().is_none());

            register_greeter(Arc::new(RealGreeter));
            assert_eq!(
                active_greeter().map(|g| g.greet()),
                Some("real".to_string())
            );

            let _ = std::thread::spawn(|| {
                let _guard = provider_slot().lock().unwrap();
                panic!("poison the registry lock");
            })
            .join();
            assert!(provider_slot().is_poisoned());
            assert_eq!(
                active_greeter().map(|g| g.greet()),
                Some("real".to_string())
            );
        }
    }

    // The declaration inventory is what turns "nobody registered this" from
    // silence into an assertable fact. Its own registry lives in a dedicated
    // module so no other test can register into it and mask the transition.
    mod declaration_inventory {
        use super::Greeter;
        use crate::provider_registry::{
            declared_provider_registries, unregistered_provider_registry_ids,
        };

        struct RealGreeter;

        impl Greeter for RealGreeter {
            fn greet(&self) -> String {
                "real".to_string()
            }
        }

        crate::provider_registry! {
            provider: dyn Greeter,
            noop: super::NoopGreeter,
            /// Register the greeter used by this test module.
            register: pub(crate) fn register_inventoried_greeter,
            with: fn with_greeter,
        }

        const ID: &str = concat!(
            module_path!(),
            "::",
            // Kept literal on purpose: the id is a contract the completeness
            // test reads, so a rename has to be a deliberate edit here too.
            "register_inventoried_greeter"
        );

        // One test: registration is a process-global one-way transition, so
        // splitting the before/after halves would make them order-dependent.
        #[test]
        fn declares_itself_and_reports_the_registration_transition() {
            // Declared without anyone having to list it anywhere.
            let declared = declared_provider_registries();
            let this = declared
                .iter()
                .find(|registry| registry.id() == ID)
                .expect("the expansion submits its own descriptor");
            assert_eq!(this.register_fn, "register_inventoried_greeter");

            // Ids are sorted and unique, so the completeness test's diff of
            // "declared but unregistered" is stable across runs.
            let ids = declared
                .iter()
                .map(|registry| registry.id())
                .collect::<Vec<_>>();
            let mut sorted = ids.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(ids, sorted, "descriptors are sorted and unique by id");

            // Before: inert, and the accessor silently yields the no-op. That
            // pairing is the bug this inventory exists to make visible.
            assert!(!this.registered());
            assert!(unregistered_provider_registry_ids().contains(&ID.to_string()));
            assert_eq!(with_greeter(|greeter| greeter.greet()), "noop");

            register_inventoried_greeter(Box::new(RealGreeter));

            // After: the same probe flips, so deleting the registration above
            // is a test failure rather than a subsystem that quietly stops.
            assert!(this.registered());
            assert!(!unregistered_provider_registry_ids().contains(&ID.to_string()));
            assert_eq!(with_greeter(|greeter| greeter.greet()), "real");
        }
    }
}
