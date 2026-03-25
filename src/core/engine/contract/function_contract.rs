//! function_contract — extracted from contract.rs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use crate::error::{Error, Result};
use super::Branch;
use super::Signature;
use super::Effect;
use super::FunctionCall;


/// A function's complete behavioral contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionContract {
    /// Function name.
    pub name: String,
    /// File path relative to component root.
    pub file: String,
    /// 1-indexed line number of the function declaration.
    pub line: usize,
    /// Function signature.
    pub signature: Signature,
    /// Distinct return paths through the function.
    pub branches: Vec<Branch>,
    /// Number of early return / guard clause statements.
    pub early_returns: usize,
    /// Aggregate side effects across all branches.
    pub effects: Vec<Effect>,
    /// Functions called within this function.
    pub calls: Vec<FunctionCall>,
    /// The type this method belongs to (from the impl block).
    /// `None` for free functions. `Some("Foo")` for `impl Foo { fn bar(&self) }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impl_type: Option<String>,
}
