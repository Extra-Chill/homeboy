//! types — extracted from contract.rs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use crate::error::{Error, Result};


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

/// Function signature: params, return type, receiver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signature {
    /// Positional parameters (excludes self/receiver).
    pub params: Vec<Param>,
    /// Return type shape.
    pub return_type: ReturnShape,
    /// Receiver (self, &self, &mut self) if this is a method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<Receiver>,
    /// Whether the function is public.
    #[serde(default)]
    pub is_public: bool,
    /// Whether the function is async.
    #[serde(default)]
    pub is_async: bool,
    /// Generic type parameters, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generics: Vec<String>,
}

/// A function parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    /// Parameter name.
    pub name: String,
    /// Type as written in source (language-specific syntax).
    #[serde(rename = "type")]
    pub param_type: String,
    /// Whether the parameter is mutable (&mut in Rust, & in PHP by-ref).
    #[serde(default)]
    pub mutable: bool,
    /// Whether the parameter has a default value.
    #[serde(default)]
    pub has_default: bool,
}

/// Return type shape — the structural pattern of what a function returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "shape")]
pub enum ReturnShape {
    /// Returns nothing (void / unit).
    #[serde(rename = "unit")]
    Unit,
    /// Returns a simple value (not Result/Option/bool).
    #[serde(rename = "value")]
    Value {
        /// Type as written in source.
        #[serde(rename = "type")]
        value_type: String,
    },
    /// Returns bool.
    #[serde(rename = "bool")]
    Bool,
    /// Returns Option<T>.
    #[serde(rename = "option")]
    OptionType {
        /// The inner type T.
        some_type: String,
    },
    /// Returns Result<T, E>.
    #[serde(rename = "result")]
    ResultType {
        /// The success type T.
        ok_type: String,
        /// The error type E.
        err_type: String,
    },
    /// Returns a collection (Vec<T>, Iterator, etc).
    #[serde(rename = "collection")]
    Collection {
        /// Element type.
        element_type: String,
    },
    /// Unrecognized return type — raw string.
    #[serde(rename = "unknown")]
    Unknown {
        /// Raw return type text.
        raw: String,
    },
}

/// Method receiver type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Receiver {
    /// `self` — takes ownership.
    OwnedSelf,
    /// `&self` — immutable borrow.
    Ref,
    /// `&mut self` — mutable borrow.
    MutRef,
}

/// A distinct execution path through the function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    /// Human-readable description of the condition that triggers this branch.
    pub condition: String,
    /// What this branch returns.
    pub returns: ReturnValue,
    /// Side effects specific to this branch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    /// Line number where this branch starts (1-indexed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

/// What a branch returns — the variant + value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReturnValue {
    /// The variant of the return type this branch produces.
    /// For Result: "ok" or "err". For Option: "some" or "none".
    /// For bool: "true" or "false". For value: "value".
    pub variant: String,
    /// A description of the returned value (e.g., "skipped", "passed", "empty vec").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// A side effect that a function may perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Effect {
    /// Reads from the filesystem.
    #[serde(rename = "file_read")]
    FileRead,
    /// Writes to the filesystem.
    #[serde(rename = "file_write")]
    FileWrite,
    /// Deletes files.
    #[serde(rename = "file_delete")]
    FileDelete,
    /// Spawns a subprocess.
    #[serde(rename = "process_spawn")]
    ProcessSpawn {
        /// The command being spawned, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    /// Mutates a parameter or self.
    #[serde(rename = "mutation")]
    Mutation {
        /// What is being mutated (e.g., "self.field", "rollback", "param_name").
        target: String,
    },
    /// Can panic (panic!, unreachable!, todo!, unwrap, expect).
    #[serde(rename = "panic")]
    Panic {
        /// The panic message or expression, if extractable.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Performs network I/O.
    #[serde(rename = "network")]
    Network,
    /// Allocates resources that need cleanup (tempfiles, locks, etc).
    #[serde(rename = "resource_alloc")]
    ResourceAlloc {
        /// Description of the resource.
        #[serde(skip_serializing_if = "Option::is_none")]
        resource: Option<String>,
    },
    /// Logs or prints output.
    #[serde(rename = "logging")]
    Logging,
}

/// A function call made within the analyzed function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// The called function name (may include module path).
    pub function: String,
    /// Parameters from the outer function that are forwarded to this call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<String>,
}

/// All contracts extracted from a single source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContracts {
    /// Relative file path.
    pub file: String,
    /// Extracted function contracts.
    pub contracts: Vec<FunctionContract>,
}

/// A type definition extracted from source code (struct, enum, class).
///
/// Language-agnostic representation of a type's structure. Used by the test
/// generator to resolve return types to their fields, enabling field-level
/// assertions instead of opaque `let _ = inner;` placeholders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDefinition {
    /// Type name (e.g., "ValidationResult", "Config").
    pub name: String,
    /// Kind: "struct", "enum", "class".
    pub kind: String,
    /// File where this type is defined (relative path).
    pub file: String,
    /// 1-indexed line number of the definition.
    pub line: usize,
    /// Fields/properties of this type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldDef>,
    /// Whether the type is public.
    #[serde(default)]
    pub is_public: bool,
}

/// A single field/property within a type definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    /// Field name.
    pub name: String,
    /// Field type as written in source.
    #[serde(rename = "type")]
    pub field_type: String,
    /// Whether the field is public.
    #[serde(default)]
    pub is_public: bool,
}
