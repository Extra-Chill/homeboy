//! universal_wrappers_always — extracted from config.rs.

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


        pub fn load(id: &str) -> Result<$Entity> {
            config::load::<$Entity>(id)
        }

        pub fn list() -> Result<Vec<$Entity>> {
            config::list::<$Entity>()
        }

        pub fn save(entity: &$Entity) -> Result<()> {
            config::save(entity)
        }

        pub fn delete(id: &str) -> Result<()> {
            config::delete::<$Entity>(id)
        }

        pub fn exists(id: &str) -> bool {
            config::exists::<$Entity>(id)
        }

        pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<RemoveResult> {
            config::remove_from_json::<$Entity>(id, json_spec)
        }

        pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<$Entity>> {
            config::create::<$Entity>(json_spec, skip_existing)
        }

        pub fn rename(id: &str, new_id: &str) -> Result<$Entity> {
            config::rename::<$Entity>(id, new_id)?;
            let resolved_id = new_id.to_lowercase();
            <$Entity as config::ConfigEntity>::on_rename(id, &resolved_id)?;
            load(&resolved_id)
        }

        pub fn delete_safe(id: &str) -> Result<()> {
            config::delete_safe::<$Entity>(id)
        }
