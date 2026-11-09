//! Tests for FFI edge cases and error paths

use cuengine::CStringPtr;
use std::error::Error;
use std::ffi::CString;
use std::path::Path;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn create_module(root: &Path) -> TestResult {
    std::fs::create_dir_all(root.join("cue.mod"))?;
    std::fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"test.example/ffi@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    Ok(())
}

#[test]
fn test_cstring_ptr_invalid_utf8() {
    let bytes = [0xff_u8, 0xfe, 0xfd, 0x00];
    // SAFETY: bytes is a live, NUL-terminated C string for the duration of
    // the call. strdup returns an independent C allocation.
    #[allow(unsafe_code)]
    let ptr = unsafe { libc::strdup(bytes.as_ptr().cast()) };
    assert!(!ptr.is_null(), "libc::strdup returned null");
    // SAFETY: ptr is a C allocation and ownership passes to the wrapper,
    // whose drop uses the compatible C free function even if the test panics.
    #[allow(unsafe_code)]
    let wrapper = unsafe { CStringPtr::new(ptr) };

    // SAFETY: wrapper owns a non-null, unchanged NUL-terminated C string;
    // `to_str` validates the deliberately invalid UTF-8 before borrowing it.
    #[allow(unsafe_code)]
    let result = unsafe { wrapper.to_str() };
    assert!(result.is_err());
}

#[test]
fn test_cstring_ptr_null_handling() {
    // SAFETY: CStringPtr accepts null and its destructor does not free it.
    #[allow(unsafe_code)]
    let null_wrapper = unsafe { CStringPtr::new(std::ptr::null_mut()) };
    assert!(null_wrapper.is_null());

    // Drop should handle null gracefully (no crash)
}

#[test]
fn test_evaluate_cue_package_null_bytes_in_path() {
    let result = cuengine::evaluate_cue_package(Path::new("invalid\0path"), "test");
    assert!(result.is_err());
}

#[test]
fn test_evaluate_cue_package_error_prefix_response() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;
    std::fs::write(
        temp_dir.path().join("invalid.cue"),
        "package test\n\nthis is { completely invalid CUE syntax @#$%",
    )?;
    let error = cuengine::evaluate_cue_package(temp_dir.path(), "test").unwrap_err();
    assert!(!error.to_string().is_empty());
    Ok(())
}

#[test]
fn test_evaluate_cue_package_various_names() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;
    for name in ["a".repeat(1000), "test_package_123".to_string()] {
        std::fs::write(
            temp_dir.path().join("env.cue"),
            format!("package {name}\nanswer: 42\n"),
        )?;
        let json = cuengine::evaluate_cue_package(temp_dir.path(), &name)?;
        let value: serde_json::Value = serde_json::from_str(&json)?;
        assert_eq!(value["answer"], 42);
    }
    Ok(())
}

#[test]
fn test_cstring_ptr_drop_with_valid_string() -> TestResult {
    // Test that drop properly frees memory for valid strings
    // This is mainly for coverage of the Drop implementation

    for content in &["test", "", "multi\nline\nstring", "unicode: 你好"] {
        let wrapper = c_allocated_cstring_ptr(content)?;

        // SAFETY: the helper returns an owned, non-null C string that remains
        // unchanged for the duration of this borrow.
        #[allow(unsafe_code)]
        let result = unsafe { wrapper.to_str()? };
        assert_eq!(result, *content);
    }
    Ok(())
}

fn c_allocated_cstring_ptr(value: &str) -> TestResult<CStringPtr> {
    let c_string = CString::new(value)?;
    // SAFETY: c_string is a valid C string that lives through the call;
    // strdup returns an independent C allocation.
    #[allow(unsafe_code)]
    let ptr = unsafe { libc::strdup(c_string.as_ptr()) };
    assert!(!ptr.is_null(), "libc::strdup returned null");
    // SAFETY: the non-null strdup allocation is transferred to the wrapper,
    // which releases it using the compatible C free function.
    #[allow(unsafe_code)]
    Ok(unsafe { CStringPtr::new(ptr) })
}

#[test]
fn test_evaluate_cue_empty_directory_returns_error() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;
    let error = cuengine::evaluate_cue_package(temp_dir.path(), "nonexistent").unwrap_err();
    assert!(!error.to_string().is_empty());
    Ok(())
}

#[test]
fn test_evaluate_cue_with_nonmatching_package() -> TestResult {
    let temp_dir = TempDir::new()?;
    create_module(temp_dir.path())?;
    std::fs::write(
        temp_dir.path().join("broken.cue"),
        "package wrong\nvalue: true\n",
    )?;
    let error = cuengine::evaluate_cue_package(temp_dir.path(), "different").unwrap_err();
    assert!(!error.to_string().is_empty());
    Ok(())
}
