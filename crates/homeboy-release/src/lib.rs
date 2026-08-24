//! Release subsystem for homeboy: versioning, changelog finalization, tagging,
//! and GitHub releases.
//!
//! Deploy (build + ship components to targets) used to live in this crate under
//! `deploy/`, on the stated grounds that release and deploy were "mutually
//! dependent". They were — but the dependency ran through a small set of shared
//! primitives (version reading, changelog mechanics, release tag naming), not
//! through the subsystems themselves. Those primitives now live in
//! `homeboy-version`, which sits below both, so the edge is one-way:
//!
//! ```text
//! homeboy-release  ->  homeboy-deploy  ->  homeboy-version  ->  homeboy-core
//! ```
//!
//! Core still reaches release/deploy behavior only through the
//! `homeboy_core::release_provider` hook, implemented here in `provider_impl`.

pub mod release;

pub use release::provider_impl;
