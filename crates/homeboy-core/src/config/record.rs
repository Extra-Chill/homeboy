use serde::{Deserialize, Serialize};

use crate::Result;

use super::{SetName, SlugIdentifiable};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record<T> {
    pub id: String,
    pub config: T,
}

impl<T: SlugIdentifiable + SetName> Record<T> {
    pub fn new(name: String, mut config: T) -> Result<Self> {
        config.set_name(name);
        let id = config.slug_id()?;
        Ok(Self { id, config })
    }
}
