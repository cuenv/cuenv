//! Regression tests for `CStringPtr` thread-safety marker fixtures

#![allow(unsafe_code)]

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
fn cstring_ptr_marker_preserves_string_access() -> TestResult {
    let cstring_ptr = c_allocated_cstring_ptr("test")?;

    assert!(!cstring_ptr.is_null());
    let result = unsafe { cstring_ptr.to_str()? };
    assert_eq!(result, "test");
    Ok(())
}

fn c_allocated_cstring_ptr(value: &str) -> Result<CStringPtr, std::ffi::NulError> {
    let c_string = CString::new(value)?;
    let ptr = unsafe { libc::strdup(c_string.as_ptr()) };
    assert!(!ptr.is_null(), "libc::strdup returned null");
    Ok(unsafe { CStringPtr::new(ptr) })
}
