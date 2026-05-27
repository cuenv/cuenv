//! Regression tests for `CStringPtr` thread-safety marker fixtures

use cuengine::CStringPtr;
use std::error::Error;
use std::ffi::CString;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn cstring_ptr_marker_allows_owned_pointer_lifecycle() -> TestResult {
    let cstring_ptr = c_allocated_cstring_ptr("test")?;
    drop(cstring_ptr);
    Ok(())
}

#[test]
#[allow(unsafe_code)]
fn cstring_ptr_marker_preserves_string_access() -> TestResult {
    let cstring_ptr = c_allocated_cstring_ptr("test")?;

    assert!(!cstring_ptr.is_null());
    let result = unsafe { cstring_ptr.to_str()? };
    assert_eq!(result, "test");
    Ok(())
}

#[allow(unsafe_code)]
fn c_allocated_cstring_ptr(value: &str) -> Result<CStringPtr, std::ffi::NulError> {
    let c_string = CString::new(value)?;
    // SAFETY: c_string.as_ptr() is a valid, null-terminated C string for the
    // duration of this call, and strdup returns a C-allocated copy.
    let ptr = unsafe { libc::strdup(c_string.as_ptr()) };
    assert!(!ptr.is_null(), "libc::strdup returned null");
    // SAFETY: ptr was allocated by strdup, is non-null, and is transferred to
    // CStringPtr so Drop frees it through the FFI string-free boundary.
    Ok(unsafe { CStringPtr::new(ptr) })
}
