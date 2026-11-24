//! Release subsystem for homeboy: versioning, changelog finalization, tagging,
//! and GitHub releases.
//!
//! Deploy (build + ship components to targets) lives in `homeboy-deploy`. The
//! primitives release and deploy share (version reading, changelog mechanics,
//! release tag naming) live in `homeboy-version`, which sits below both, so the
//! edge is one-way:
//!
//! ```text
//! homeboy-release  ->  homeboy-deploy  ->  homeboy-version  ->  homeboy-core
//! ```
//!
//! Core still reaches release/deploy behavior only through the
//! `homeboy_core::release_provider` hook, implemented here in `provider_impl`.

pub mod release;

pub use release::provider_impl;
pub use release::register_action_delegate;
