//! Test builders for `cook_delivery` fixtures: one shared way to give a run
//! the record the single writer would persist.

use serde_json::Value;

use super::*;

/// Test builder: record `edit`'s artifacts in `metadata`'s `cook_delivery`
/// record, with the facts the single writer would project from them.
pub fn edit_cook_delivery_for_test(
    metadata: &mut Value,
    edit: impl FnOnce(&mut CookDeliveryArtifacts),
) {
    let mut artifacts = CookDeliveryArtifacts::from_run_metadata(metadata);
    edit(&mut artifacts);
    let recorded = CookDelivery::from_run_metadata(metadata);
    let delivery = CookDelivery::project(&artifacts, recorded.as_ref());
    metadata[COOK_DELIVERY_KEY] = CookDelivery::to_record(delivery.as_ref(), &artifacts);
}

/// Test builder: set the artifact at `pointer` (a JSON pointer into
/// [`CookDeliveryArtifacts`], e.g. `/promotion/status`) and reproject.
pub fn set_cook_delivery_artifact_for_test(metadata: &mut Value, pointer: &str, value: Value) {
    edit_cook_delivery_for_test(metadata, |artifacts| {
        let mut json = serde_json::to_value(&*artifacts).expect("Cook delivery artifacts");
        let slot =
            pointer
                .split('/')
                .skip(1)
                .fold(&mut json, |slot, key| match key.parse::<usize>() {
                    Ok(index) if slot.is_array() => &mut slot[index],
                    _ => &mut slot[key],
                });
        *slot = value;
        *artifacts = serde_json::from_value(json).expect("Cook delivery artifacts");
    });
}

/// Test builder: record `blocker` on the run's delivery, as
/// `record_cook_delivery_blocker_in_store` does.
pub fn record_cook_delivery_blocker_for_test(
    metadata: &mut Value,
    code: CookDeliveryBlockerCode,
    message: &str,
    next_action: Option<&str>,
    base_movements: u32,
) {
    let artifacts = CookDeliveryArtifacts::from_run_metadata(metadata);
    let recorded = CookDelivery::from_run_metadata(metadata);
    let mut delivery = CookDelivery::project(&artifacts, recorded.as_ref())
        .or(recorded)
        .unwrap_or(CookDelivery {
            state: CookDeliveryState::Blocked,
            candidate: None,
            destination: None,
            gates: Vec::new(),
            publication: None,
            base_movements: 0,
            blocker: None,
        });
    delivery.state = CookDeliveryState::Blocked;
    delivery.blocker = Some(CookDeliveryBlocker {
        code,
        message: message.to_string(),
        paths: Vec::new(),
        next_action: next_action.map(str::to_string),
        cause: None,
        evidence: RECORDED_BLOCKER_EVIDENCE.to_string(),
        output: None,
    });
    delivery.base_movements = base_movements;
    metadata[COOK_DELIVERY_KEY] = CookDelivery::to_record(Some(&delivery), &artifacts);
}

/// Test builder: run metadata holding only a `cook_delivery` record of
/// `artifacts` (a JSON [`CookDeliveryArtifacts`]).
pub fn cook_delivery_metadata_for_test(artifacts: Value) -> Value {
    let mut metadata = Value::Object(Default::default());
    edit_cook_delivery_for_test(&mut metadata, |recorded| {
        *recorded = serde_json::from_value(artifacts).expect("Cook delivery artifacts");
    });
    metadata
}
