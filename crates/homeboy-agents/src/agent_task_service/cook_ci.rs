//! Durable CI verification receipt, separate from pre-publication gate evidence.
use super::cook_recipe::CookRecipeStore;
use homeboy_core::git::cook_ci::{poll_cook_ci, GithubCookCi};
use homeboy_core::{Error, Result};
use serde_json::{json, Value};

pub fn cook_ci_status(id: &str) -> Result<Option<Value>> {
    let store = CookRecipeStore::from_current_data_root()?;
    let recipe = store
        .load_recipe(id)
        .ok()
        .or_else(|| store.load_recipe_for_attempt(id).ok().flatten());
    let Some(recipe) = recipe else {
        return Ok(None);
    };
    let path = store.ci_path(&recipe.cook_id);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| Error::internal_unexpected(e.to_string()))?;
    Ok(Some(
        serde_json::from_slice(&bytes).map_err(|e| Error::internal_unexpected(e.to_string()))?,
    ))
}

pub fn repoll_cook_ci(id: &str) -> Result<Option<Value>> {
    let Some(receipt) = cook_ci_status(id)? else {
        return Ok(None);
    };
    let store = CookRecipeStore::from_current_data_root()?;
    let cook_id = receipt["cook_id"].as_str().unwrap_or(id);
    let _driver = store.try_acquire_cook_driver(cook_id)?.ok_or_else(|| {
        Error::internal_unexpected("Cook is owned by another controller; poll again later")
    })?;
    let url = receipt["pr_url"]
        .as_str()
        .ok_or_else(|| Error::internal_unexpected("CI receipt missing PR URL"))?;
    let evidence = poll_cook_ci(&GithubCookCi, url)?;
    let status = if evidence.status == "verified" {
        "completed"
    } else {
        &evidence.status
    };
    let value =
        json!({"cook_id":receipt["cook_id"], "pr_url":url, "status":status, "evidence":evidence});
    save(&store, cook_id, &value)?;
    Ok(Some(value))
}

pub(crate) fn save(store: &CookRecipeStore, cook_id: &str, value: &Value) -> Result<()> {
    let path = store.ci_path(cook_id);
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(value).expect("JSON receipt"),
    )
    .and_then(|_| std::fs::rename(&temp, &path))
    .map_err(|e| Error::internal_unexpected(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cook_ci_receipt_is_durable_and_replaceable() {
        let root = std::env::temp_dir().join(format!("cook-ci-{}", uuid::Uuid::new_v4()));
        let store = CookRecipeStore::from_data_root(root.clone());
        let path = store.ci_path("cook");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for status in ["awaiting_ci", "ci_gate_failed", "completed"] {
            let receipt = json!({"cook_id":"cook", "pr_url":"https://github.com/o/r/pull/1", "status":status});
            save(&store, "cook", &receipt).unwrap();
            let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(restored, receipt);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
