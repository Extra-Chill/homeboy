//! Small validation helpers shared by the lifecycle and resource modules.

use crate::error::{Error, Result};

/// Reject a blank required field.
///
/// Blank-string rejection is identical wherever it appears, so it is declared
/// once here rather than restated per module.
pub fn validate_required_field(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "must not be blank",
            None,
            None,
        ));
    }

    Ok(())
}
