//! optional_features — extracted from config.rs.

use crate::engine::identifier;
use crate::output::{
    BatchResult, CreateOutput, CreateResult, MergeOutput, MergeResult, RemoveResult,
};
use crate::Result;
use crate::engine::local_files::{self, FileSystem};
use crate::error::Error;
use heck::ToSnakeCase;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::Read;
use std::path::{Path, PathBuf};


        pub fn list_ids() -> Result<Vec<String>> {
            config::list_ids::<$Entity>()
        }

        pub fn merge(
            id: Option<&str>,
            json_spec: &str,
            replace_fields: &[String],
        ) -> Result<MergeOutput> {
            config::merge::<$Entity>(id, json_spec, replace_fields)
        }

        pub fn slugify_id(name: &str) -> Result<String> {
            crate::engine::identifier::slugify_id(name, "name")
        }
