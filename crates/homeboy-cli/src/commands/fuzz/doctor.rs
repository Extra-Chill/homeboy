use homeboy::core::extension::{
    check_update_available, extension_ready_status, is_extension_linked, load_extension,
    read_source_revision,
};

use super::types::FuzzDoctorArgs;
use super::types_extra::{FuzzDoctorExtensionOutput, FuzzDoctorHomeboyOutput, FuzzDoctorOutput};

pub(super) fn run_doctor(args: FuzzDoctorArgs) -> homeboy::core::Result<FuzzDoctorOutput> {
    let extension = load_extension(&args.extension_id)?;
    let ready = extension_ready_status(&extension);
    let linked = is_extension_linked(&extension.id);
    let source_revision = read_source_revision(&extension.id);
    let update = check_update_available(&extension.id);
    let update_command = format!("homeboy extension update {}", extension.id);
    let status = if ready.ready && update.is_none() {
        "ok"
    } else {
        "attention"
    };

    Ok(FuzzDoctorOutput {
        command: "fuzz.doctor".to_string(),
        status: status.to_string(),
        homeboy: FuzzDoctorHomeboyOutput {
            controller_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        extension: FuzzDoctorExtensionOutput {
            id: extension.id.clone(),
            name: extension.name.clone(),
            version: extension.version.clone(),
            path: extension.extension_path.clone().unwrap_or_default(),
            linked,
            ready: ready.ready,
            ready_reason: ready.reason,
            ready_detail: ready.detail,
            source_url: extension.source_url.clone(),
            source_revision,
            commits_behind: update.map(|update| update.behind_count),
            update_command,
        },
        update_hint: "Update source checkouts and the active installed extension; fuzz runs use the installed extension/runtime, not an arbitrary source checkout.".to_string(),
    })
}
