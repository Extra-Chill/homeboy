//! OS keychain storage for project API variables.
//!
//! Values are stored under service `homeboy` with account
//! `<project-id>:<variable-name>`.

use crate::core::error::{Error, ErrorCode, Result};
use keyring::Entry;
use serde_json::{json, Value};

const SERVICE_NAME: &str = "homeboy";

fn keyring_error(e: keyring::Error) -> Error {
    Error::new(
        ErrorCode::InternalUnexpected,
        format!("Keychain error: {}", e),
        json!({ "error": e.to_string() }),
    )
    .with_hint("Use source: \"env\" for CI/headless environments, or unlock/configure the OS keychain for local use")
}

fn account_key(project_id: &str, variable_name: &str) -> String {
    format!("{}:{}", project_id, variable_name)
}

fn entry(project_id: &str, variable_name: &str) -> Result<Entry> {
    Entry::new(SERVICE_NAME, &account_key(project_id, variable_name)).map_err(keyring_error)
}

/// Stores a project API variable in the OS keychain.
pub fn set(project_id: &str, variable_name: &str, value: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return macos::set(project_id, variable_name, value);
    }

    #[cfg(not(target_os = "macos"))]
    entry(project_id, variable_name)?
        .set_password(value)
        .map_err(keyring_error)
}

/// Retrieves a project API variable from the OS keychain.
pub fn get(project_id: &str, variable_name: &str) -> Result<Option<String>> {
    match entry(project_id, variable_name)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(keyring_error(e)),
    }
}

/// Removes a project API variable from the OS keychain.
pub fn remove(project_id: &str, variable_name: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return macos::remove(project_id, variable_name);
    }

    #[cfg(not(target_os = "macos"))]
    match entry(project_id, variable_name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(keyring_error(e)),
    }
}

/// Checks whether a project API variable is present in the OS keychain.
pub fn exists(project_id: &str, variable_name: &str) -> bool {
    get(project_id, variable_name)
        .map(|value| value.is_some())
        .unwrap_or(false)
}

/// Removes the named project API variables from the OS keychain.
pub fn remove_many(project_id: &str, variable_names: &[String]) -> Result<usize> {
    let mut removed = 0;
    for variable_name in variable_names {
        if get(project_id, variable_name)?.is_some() {
            remove(project_id, variable_name)?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn missing_error(project_id: &str, variable_name: &str) -> Error {
    Error::new(
        ErrorCode::ExtensionNotFound,
        format!(
            "Keychain variable '{}' is not set for project '{}'",
            variable_name, project_id
        ),
        Value::Null,
    )
    .with_hint(format!(
        "Run 'homeboy auth set --project {} {}' to store it locally",
        project_id, variable_name
    ))
    .with_hint("Use source: \"env\" instead for CI/headless environments")
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{account_key, SERVICE_NAME};
    use crate::core::error::{Error, ErrorCode, Result};
    use core_foundation_sys::array::{kCFTypeArrayCallBacks, CFArrayCreate};
    use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease, CFTypeRef, OSStatus};
    use core_foundation_sys::string::{kCFStringEncodingUTF8, CFStringCreateWithCString};
    use security_framework_sys::base::{errSecDuplicateItem, errSecSuccess, SecAccessRef};
    use security_framework_sys::base::{SecKeychainItemRef, SecKeychainRef};
    use security_framework_sys::keychain::{
        SecKeychainAddGenericPassword, SecKeychainFindGenericPassword,
    };
    use security_framework_sys::keychain_item::{
        SecKeychainItemDelete, SecKeychainItemModifyAttributesAndData,
    };
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::ptr;

    const ERR_SEC_NO_ACCESS_FOR_ITEM: OSStatus = -25320;

    enum OpaqueSecTrustedApplicationRef {}
    type SecTrustedApplicationRef = *mut OpaqueSecTrustedApplicationRef;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecAccessCreate(
            descriptor: core_foundation_sys::string::CFStringRef,
            trustedlist: core_foundation_sys::array::CFArrayRef,
            accessRef: *mut SecAccessRef,
        ) -> OSStatus;
        fn SecKeychainItemSetAccess(itemRef: SecKeychainItemRef, access: SecAccessRef) -> OSStatus;
        fn SecTrustedApplicationCreateFromPath(
            path: *const c_char,
            app: *mut SecTrustedApplicationRef,
        ) -> OSStatus;
    }

    pub fn set(project_id: &str, variable_name: &str, value: &str) -> Result<()> {
        let account = account_key(project_id, variable_name);
        let mut item = ptr::null_mut();
        let add_status = unsafe {
            SecKeychainAddGenericPassword(
                ptr::null_mut::<std::ffi::c_void>() as SecKeychainRef,
                SERVICE_NAME.len() as u32,
                SERVICE_NAME.as_ptr().cast(),
                account.len() as u32,
                account.as_ptr().cast(),
                value.len() as u32,
                value.as_ptr().cast(),
                &mut item,
            )
        };

        if add_status == errSecDuplicateItem {
            item = find_item(&account)?;
            cvt(
                unsafe {
                    SecKeychainItemModifyAttributesAndData(
                        item,
                        ptr::null(),
                        value.len() as u32,
                        value.as_ptr().cast(),
                    )
                },
                "update keychain item",
            )?;
        } else {
            cvt(add_status, "add keychain item")?;
        }

        set_current_exe_access(item)?;
        unsafe { CFRelease(item as CFTypeRef) };
        Ok(())
    }

    pub fn remove(project_id: &str, variable_name: &str) -> Result<()> {
        let account = account_key(project_id, variable_name);
        let item = match find_item(&account) {
            Ok(item) => item,
            Err(error)
                if error
                    .details
                    .get("status")
                    .and_then(|status| status.as_i64())
                    == Some(-25300) =>
            {
                return Ok(())
            }
            Err(error) => return Err(error),
        };
        let result = cvt(
            unsafe { SecKeychainItemDelete(item) },
            "delete keychain item",
        );
        unsafe { CFRelease(item as CFTypeRef) };
        result
    }

    fn find_item(account: &str) -> Result<SecKeychainItemRef> {
        let mut item = ptr::null_mut();
        cvt(
            unsafe {
                SecKeychainFindGenericPassword(
                    ptr::null(),
                    SERVICE_NAME.len() as u32,
                    SERVICE_NAME.as_ptr().cast(),
                    account.len() as u32,
                    account.as_ptr().cast(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut item,
                )
            },
            "find keychain item",
        )?;
        Ok(item)
    }

    fn set_current_exe_access(item: SecKeychainItemRef) -> Result<()> {
        let exe = std::env::current_exe().map_err(|e| {
            Error::new(
                ErrorCode::InternalUnexpected,
                format!(
                    "Keychain error: could not resolve current executable: {}",
                    e
                ),
                serde_json::json!({ "error": e.to_string() }),
            )
        })?;
        let exe_path = CString::new(exe.as_os_str().as_bytes()).map_err(|e| {
            Error::new(
                ErrorCode::InternalUnexpected,
                format!("Keychain error: executable path contains a NUL byte: {}", e),
                serde_json::json!({ "error": e.to_string() }),
            )
        })?;

        let mut trusted_app = ptr::null_mut();
        cvt(
            unsafe { SecTrustedApplicationCreateFromPath(exe_path.as_ptr(), &mut trusted_app) },
            "create trusted application",
        )?;

        let trusted_values = [trusted_app as *const c_void];
        let trusted_list = unsafe {
            CFArrayCreate(
                kCFAllocatorDefault,
                trusted_values.as_ptr(),
                trusted_values.len() as isize,
                &kCFTypeArrayCallBacks,
            )
        };
        if trusted_list.is_null() {
            unsafe { CFRelease(trusted_app as CFTypeRef) };
            return Err(security_error(0, "create trusted application list"));
        }

        let descriptor = unsafe {
            CFStringCreateWithCString(
                kCFAllocatorDefault,
                b"homeboy\0".as_ptr().cast(),
                kCFStringEncodingUTF8,
            )
        };
        if descriptor.is_null() {
            unsafe {
                CFRelease(trusted_list as CFTypeRef);
                CFRelease(trusted_app as CFTypeRef);
            }
            return Err(security_error(0, "create keychain access descriptor"));
        }

        let mut access = ptr::null_mut();
        let access_status = unsafe { SecAccessCreate(descriptor, trusted_list, &mut access) };
        unsafe {
            CFRelease(descriptor as CFTypeRef);
            CFRelease(trusted_list as CFTypeRef);
            CFRelease(trusted_app as CFTypeRef);
        }
        cvt(access_status, "create keychain access control")?;

        let set_status = unsafe { SecKeychainItemSetAccess(item, access) };
        unsafe { CFRelease(access as CFTypeRef) };
        if set_status == ERR_SEC_NO_ACCESS_FOR_ITEM {
            return Ok(());
        }
        cvt(set_status, "set keychain item access")
    }

    fn cvt(status: OSStatus, action: &str) -> Result<()> {
        if status == errSecSuccess {
            Ok(())
        } else {
            Err(security_error(status, action))
        }
    }

    fn security_error(status: OSStatus, action: &str) -> Error {
        Error::new(
            ErrorCode::InternalUnexpected,
            format!("Keychain error: failed to {} ({})", action, status),
            serde_json::json!({ "status": status, "action": action }),
        )
        .with_hint("Use source: \"env\" for CI/headless environments, or unlock/configure the OS keychain for local use")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_key_uses_project_and_variable() {
        assert_eq!(account_key("wpcloud-api", "token"), "wpcloud-api:token");
    }

    #[test]
    fn test_missing_error() {
        let err = missing_error("wpcloud-api", "token");

        assert!(err.message.contains("token"));
        assert!(err.message.contains("wpcloud-api"));
        assert_eq!(err.hints.len(), 2);
    }

    #[test]
    #[ignore]
    fn test_set() {
        let project_id = "homeboy-keychain-test-set";
        let variable_name = "token";

        remove(project_id, variable_name).expect("clean old value");
        set(project_id, variable_name, "secret-value").expect("store value");
        assert_eq!(
            get(project_id, variable_name)
                .expect("read value")
                .as_deref(),
            Some("secret-value")
        );
        remove(project_id, variable_name).expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_get() {
        let project_id = "homeboy-keychain-test-get";
        let variable_name = "token";

        remove(project_id, variable_name).expect("clean old value");
        assert_eq!(
            get(project_id, variable_name).expect("read missing value"),
            None
        );
        set(project_id, variable_name, "secret-value").expect("store value");
        assert_eq!(
            get(project_id, variable_name)
                .expect("read value")
                .as_deref(),
            Some("secret-value")
        );
        remove(project_id, variable_name).expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_remove() {
        let project_id = "homeboy-keychain-test-remove";
        let variable_name = "token";

        set(project_id, variable_name, "secret-value").expect("store value");
        remove(project_id, variable_name).expect("remove value");
        assert_eq!(
            get(project_id, variable_name).expect("read missing value"),
            None
        );
    }

    #[test]
    #[ignore]
    fn test_exists() {
        let project_id = "homeboy-keychain-test-exists";
        let variable_name = "token";

        remove(project_id, variable_name).expect("clean old value");
        assert!(!exists(project_id, variable_name));
        set(project_id, variable_name, "secret-value").expect("store value");
        assert!(exists(project_id, variable_name));
        remove(project_id, variable_name).expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_remove_many() {
        let project_id = "homeboy-keychain-test-remove-many";
        let variables = vec!["token".to_string(), "refresh".to_string()];

        set(project_id, "token", "secret-value").expect("store token");
        set(project_id, "refresh", "refresh-value").expect("store refresh");
        assert_eq!(
            remove_many(project_id, &variables).expect("remove values"),
            2
        );
        assert!(!exists(project_id, "token"));
        assert!(!exists(project_id, "refresh"));
    }

    #[test]
    #[ignore]
    fn stores_reads_and_removes_keychain_value() {
        let project_id = "homeboy-keychain-test";
        let variable_name = "token";
        let value = "secret-value";

        set(project_id, variable_name, value).expect("store value");
        assert_eq!(
            get(project_id, variable_name).expect("read value"),
            Some(value.to_string())
        );
        remove(project_id, variable_name).expect("remove value");
        assert_eq!(
            get(project_id, variable_name).expect("read missing value"),
            None
        );
    }
}
