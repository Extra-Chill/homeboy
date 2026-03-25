//! types — extracted from core_fingerprint.rs.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use sha2::{Digest, Sha256};
use crate::extension::grammar::{self, Grammar, Symbol};
use crate::extension::{self, DeadCodeMarker, HookRef, UnusedParam};
use super::super::conventions::Language;
use super::super::fingerprint::FileFingerprint;


/// A function extracted from source with full context.
pub(crate) struct FunctionInfo {
    name: String,
    body: String,
    visibility: String,
    is_test: bool,
    is_trait_impl: bool,
    params: String,
    _start_line: usize,
}

pub(crate) struct ImplContext {
    line: usize,
    depth: i32,
    _type_name: String,
    trait_name: Option<String>,
}
