use std::ffi::{CString, OsStr};
use std::fs;
use std::os::raw::c_char;
use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;

use super::{
    BridgeError, CStringPtr, DEFAULT_MODULE_EVAL_TIMEOUT, ERROR_CODE_BUILD_VALUE,
    ERROR_CODE_DEPENDENCY_RES, ERROR_CODE_INVALID_INPUT, ERROR_CODE_JSON_MARSHAL,
    ERROR_CODE_LOAD_INSTANCE, ERROR_CODE_ORDERED_JSON, ERROR_CODE_PANIC_RECOVER,
    ERROR_CODE_REGISTRY_INIT, EVAL_TIMEOUT_ENV_VAR, ModuleResult, Result,
};
use crate::CueEngineError;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn create_test_module(root: &Path) {
    fs::create_dir_all(root.join("cue.mod")).unwrap();
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"test.example/ffi@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )
    .unwrap();
}

#[test]
fn test_cstring_ptr_creation() -> TestResult {
    // Test with null pointer
    let null_ptr = null_cstring_ptr();
    assert!(null_ptr.is_null());

    // Test with non-null pointer allocated like the Go bridge does.
    let wrapper = c_allocated_cstring_ptr("test")?;
    assert!(!wrapper.is_null());

    // Convert back to string and verify
    let result_str = cstring_ptr_to_str(&wrapper)?;
    assert_eq!(result_str, "test");
    // CStringPtr will automatically free the memory when dropped
    Ok(())
}

#[test]
fn test_cstring_ptr_utf8_conversion() -> TestResult {
    let test_content = "Hello, 世界! 🦀";
    let wrapper = c_allocated_cstring_ptr(test_content)?;

    let converted = cstring_ptr_to_str(&wrapper)?;
    assert_eq!(converted, test_content);
    Ok(())
}

#[test]
fn test_cstring_ptr_empty_string() -> TestResult {
    let wrapper = c_allocated_cstring_ptr("")?;

    assert!(!wrapper.is_null());
    let result = cstring_ptr_to_str(&wrapper)?;
    assert_eq!(result, "");
    Ok(())
}

#[test]
fn test_extract_ffi_string_rejects_null() {
    let null_wrapper = null_cstring_ptr();
    assert!(null_wrapper.is_null());
    assert!(super::extract_ffi_string(&null_wrapper, "test").is_err());
}

#[test]
fn test_evaluate_cue_package_invalid_path() {
    // Test with invalid UTF-8 path (simulated)
    let invalid_path = Path::new("/nonexistent/\u{0000}/invalid");
    let result = super::evaluate_cue_package(invalid_path, "test");

    // Should fail with configuration error for invalid path
    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("FFI operation failed"));
}

#[test]
fn evaluate_cue_file_preserves_module_imports_and_isolates_siblings()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir_all(root.path().join("cue.mod"))?;
    std::fs::create_dir_all(root.path().join("shared"))?;
    std::fs::write(
        root.path().join("cue.mod/module.cue"),
        "module: \"example.org/rules@v0\"\nlanguage: version: \"v0.14.0\"\n",
    )?;
    std::fs::write(
        root.path().join("shared/schema.cue"),
        "package shared\n#Rules: {answer: 42}\n",
    )?;
    std::fs::write(root.path().join("env.cue"), "package cuenv\nanswer: 1\n")?;
    let file = root.path().join(".rules.cue");
    std::fs::write(
        &file,
        "package cuenv\nimport \"example.org/rules/shared\"\nshared.#Rules\n",
    )?;
    assert_eq!(
        super::evaluate_cue_file(&file)?,
        serde_json::json!({"answer": 42})
    );
    Ok(())
}

#[test]
fn test_evaluate_cue_package_invalid_package_name() {
    let temp_dir = TempDir::new().unwrap();

    // Package name with null bytes should fail
    let result = super::evaluate_cue_package(temp_dir.path(), "test\0package");

    assert!(result.is_err());
    let error = result.unwrap_err();
    assert!(error.to_string().contains("FFI operation failed"));
}

#[test]
fn test_evaluate_cue_package_nonexistent_directory() {
    let nonexistent = Path::new("/definitely/does/not/exist/12345");
    let error = super::evaluate_cue_package(nonexistent, "env").unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[test]
fn test_evaluate_cue_package_with_valid_setup() {
    let temp_dir = TempDir::new().unwrap();
    create_test_module(temp_dir.path());
    fs::write(
        temp_dir.path().join("env.cue"),
        "package cuenv\nenv: {TEST_VAR: \"test_value\", NUMBER: 42}\n",
    )
    .unwrap();
    let json = super::evaluate_cue_package(temp_dir.path(), "cuenv").unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["env"]["TEST_VAR"], "test_value");
    assert_eq!(value["env"]["NUMBER"], 42);
}

#[test]
fn test_evaluate_cue_error_handling() {
    let temp_dir = TempDir::new().unwrap();
    create_test_module(temp_dir.path());
    fs::write(
        temp_dir.path().join("env.cue"),
        "package cuenv\nthis is not valid CUE syntax {\n",
    )
    .unwrap();
    let error = super::evaluate_cue_package(temp_dir.path(), "cuenv").unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[test]
fn test_path_conversion_edge_cases() {
    let temp_dir = TempDir::new().unwrap();
    create_test_module(temp_dir.path());
    let path_with_spaces = temp_dir.path().join("dir with spaces");
    fs::create_dir(&path_with_spaces).unwrap();
    fs::write(
        path_with_spaces.join("env.cue"),
        "package env\nanswer: 42\n",
    )
    .unwrap();
    let json = super::evaluate_cue_package(&path_with_spaces, "env").unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["answer"], 42);
}

#[test]
fn test_ffi_memory_management_stress() {
    let temp_dir = TempDir::new().unwrap();
    create_test_module(temp_dir.path());
    fs::write(
        temp_dir.path().join("env.cue"),
        "package cuenv\nenv: {TEST: \"value\"}\n",
    )
    .unwrap();
    for _ in 0..100 {
        let json = super::evaluate_cue_package(temp_dir.path(), "cuenv").unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["env"]["TEST"], "value");
    }
}

#[test]
fn test_get_bridge_version() {
    let version = super::get_bridge_version().unwrap();
    assert!(
        version.starts_with("bridge/1"),
        "unexpected bridge version: {version}"
    );
    assert!(version.contains("Go "), "missing Go version: {version}");
}

#[test]
fn test_error_message_parsing() {
    let temp_dir = TempDir::new().unwrap();
    create_test_module(temp_dir.path());
    fs::write(
        temp_dir.path().join("env.cue"),
        "package cuenv\nanswer: 42\n",
    )
    .unwrap();
    let error = super::evaluate_cue_package(temp_dir.path(), "nonexistent_package").unwrap_err();
    assert!(!error.to_string().is_empty());
}

#[test]
fn test_bridge_error_constants_consistency() {
    // Test that our error constants match expected values
    assert_eq!(ERROR_CODE_INVALID_INPUT, "INVALID_INPUT");
    assert_eq!(ERROR_CODE_LOAD_INSTANCE, "LOAD_INSTANCE");
    assert_eq!(ERROR_CODE_BUILD_VALUE, "BUILD_VALUE");
    assert_eq!(ERROR_CODE_ORDERED_JSON, "ORDERED_JSON");
    assert_eq!(ERROR_CODE_PANIC_RECOVER, "PANIC_RECOVER");
    assert_eq!(ERROR_CODE_JSON_MARSHAL, "JSON_MARSHAL_ERROR");
    assert_eq!(ERROR_CODE_REGISTRY_INIT, "REGISTRY_INIT");
    assert_eq!(ERROR_CODE_DEPENDENCY_RES, "DEPENDENCY_RESOLUTION");
}

#[test]
fn test_bridge_envelope_parsing() {
    // Test parsing of valid success envelope
    let success_json = r#"{"version":"bridge/1","ok":{"test":"value"}}"#;
    let envelope = super::parse_bridge_envelope(success_json).unwrap();

    assert_eq!(envelope.version, "bridge/1");
    assert!(envelope.ok.is_some());
    assert!(envelope.error.is_none());

    // Test parsing of valid error envelope
    let error_json = r#"{"version":"bridge/1","error":{"code":"INVALID_INPUT","message":"test error","hint":"test hint"}}"#;
    let envelope = super::parse_bridge_envelope(error_json).unwrap();

    assert_eq!(envelope.version, "bridge/1");
    assert!(envelope.ok.is_none());
    assert!(envelope.error.is_some());

    let error = envelope.error.unwrap();
    assert_eq!(error.code, "INVALID_INPUT");
    assert_eq!(error.message, "test error");
    assert_eq!(error.hint, Some("test hint".to_string()));
}

#[test]
fn test_bridge_envelope_parsing_minimal_error() {
    // Test parsing of error envelope without hint
    let error_json =
        r#"{"version":"bridge/1","error":{"code":"LOAD_INSTANCE","message":"test error"}}"#;
    let envelope = super::parse_bridge_envelope(error_json).unwrap();

    let error = envelope.error.unwrap();
    assert_eq!(error.code, "LOAD_INSTANCE");
    assert_eq!(error.message, "test error");
    assert!(error.hint.is_none());
}

#[test]
fn test_cstring_ptr_drop_behavior() -> TestResult {
    // Test that Drop trait is correctly implemented
    // This is mostly to ensure the Drop implementation doesn't panic

    // Test dropping a null pointer (should be safe)
    let null_ptr = null_cstring_ptr();
    drop(null_ptr); // Should not panic

    // Test dropping a valid pointer
    let wrapper = c_allocated_cstring_ptr("test")?;
    drop(wrapper); // Should free the memory properly
    Ok(())
}

fn c_allocated_cstring_ptr(value: &str) -> TestResult<CStringPtr> {
    let c_string = CString::new(value)?;
    let ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to allocate a C string fixture freed through CStringPtr"
        )]
        // SAFETY: c_string stays live and NUL-terminated through the call;
        // strdup returns an independent C allocation.
        unsafe {
            libc::strdup(c_string.as_ptr())
        }
    };
    assert!(!ptr.is_null(), "libc::strdup returned null");
    Ok(c_allocated_ptr(ptr))
}

fn null_cstring_ptr() -> CStringPtr {
    c_allocated_ptr(std::ptr::null_mut())
}

fn c_allocated_ptr(ptr: *mut c_char) -> CStringPtr {
    #[expect(
        unsafe_code,
        reason = "Required to wrap C string test fixtures in CStringPtr"
    )]
    // SAFETY: the callers provide either null or an exclusively owned C
    // allocation from strdup, which the wrapper releases through C free.
    unsafe {
        CStringPtr::new(ptr)
    }
}

fn cstring_ptr_to_str(wrapper: &CStringPtr) -> Result<&str> {
    assert!(!wrapper.is_null(), "expected a non-null C string fixture");
    #[expect(
        unsafe_code,
        reason = "Required to exercise CStringPtr string conversion in unit tests"
    )]
    // SAFETY: fixture wrappers own a non-null, NUL-terminated C string, and
    // the borrowed contents stay unchanged while the wrapper is borrowed.
    unsafe {
        wrapper.to_str()
    }
}

#[test]
fn test_get_bridge_version_functionality() {
    let version = super::get_bridge_version().unwrap();
    assert!(
        version.starts_with("bridge/1"),
        "unexpected bridge version: {version}"
    );
    assert!(version.contains("Go "), "missing Go version: {version}");
}

#[test]
fn test_error_code_mapping() {
    // Test that we handle different error codes correctly
    // We can't easily mock the FFI, but we can test the logic

    // Create a mock bridge error for each error type
    let test_cases = vec![
        (ERROR_CODE_INVALID_INPUT, "Invalid input test", None),
        (
            ERROR_CODE_LOAD_INSTANCE,
            "Load instance test",
            Some("Check CUE files".to_string()),
        ),
        (
            ERROR_CODE_BUILD_VALUE,
            "Build value test",
            Some("Check constraints".to_string()),
        ),
        (ERROR_CODE_ORDERED_JSON, "JSON test", None),
        (ERROR_CODE_PANIC_RECOVER, "Panic test", None),
        (ERROR_CODE_JSON_MARSHAL, "Marshal test", None),
        (
            ERROR_CODE_REGISTRY_INIT,
            "Registry init test",
            Some("Check CUE_REGISTRY".to_string()),
        ),
        (
            ERROR_CODE_DEPENDENCY_RES,
            "Dependency resolution test",
            Some("Run 'cue mod tidy'".to_string()),
        ),
        ("UNKNOWN_CODE", "Unknown error", None),
    ];

    for (code, message, hint) in test_cases {
        let bridge_error = BridgeError {
            code: code.to_string(),
            message: message.to_string(),
            hint,
        };

        // The error should serialize and deserialize properly
        let serialized = serde_json::to_string(&bridge_error).unwrap();
        let deserialized: BridgeError = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.code, code);
        assert_eq!(deserialized.message, message);
    }
}

#[test]
fn test_path_edge_cases() {
    let temp_dir = TempDir::new().unwrap();
    create_test_module(temp_dir.path());
    fs::write(
        temp_dir.path().join("env.cue"),
        "package cuenv\nanswer: 42\n",
    )
    .unwrap();
    let json = super::evaluate_cue_package(temp_dir.path(), "").unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["answer"], 42);
}

#[test]
fn test_json_envelope_version_mismatch() {
    let incompatible_version_json = r#"{"version":"bridge/2","ok":{"test":"value"}}"#;
    let error = super::parse_bridge_envelope(incompatible_version_json).unwrap_err();
    let message = error.to_string();

    assert!(message.contains("Unsupported CUE bridge protocol version bridge/2"));
    assert!(message.contains("expected bridge/1"));
}

#[test]
fn test_serialize_import_usage() {
    // Test that the Serialize import is available even if not used
    // This ensures the import consistency we added is correct

    use serde::Serialize;

    #[derive(Serialize)]
    struct TestStruct {
        field: String,
    }

    let test = TestStruct {
        field: "test".to_string(),
    };

    let _json = serde_json::to_string(&test).unwrap();
    // If this compiles, the Serialize import is working
}

#[test]
fn test_module_eval_timeout_defaults_when_unset_or_empty() -> TestResult {
    assert_eq!(
        super::parse_module_eval_timeout(None)?,
        DEFAULT_MODULE_EVAL_TIMEOUT
    );
    assert_eq!(
        super::parse_module_eval_timeout(Some(OsStr::new("")))?,
        DEFAULT_MODULE_EVAL_TIMEOUT
    );
    assert_eq!(
        super::parse_module_eval_timeout(Some(OsStr::new("  ")))?,
        DEFAULT_MODULE_EVAL_TIMEOUT
    );
    assert_eq!(DEFAULT_MODULE_EVAL_TIMEOUT, Duration::from_secs(60));
    Ok(())
}

#[test]
fn test_module_eval_timeout_parses_whole_seconds() -> TestResult {
    assert_eq!(
        super::parse_module_eval_timeout(Some(OsStr::new("1")))?,
        Duration::from_secs(1)
    );
    assert_eq!(
        super::parse_module_eval_timeout(Some(OsStr::new(" 120 ")))?,
        Duration::from_secs(120)
    );
    Ok(())
}

#[test]
fn test_module_eval_timeout_rejects_invalid_values() {
    for value in ["0", "-5", "abc", "1.5", "10s", "18446744073709551616"] {
        let err = super::parse_module_eval_timeout(Some(OsStr::new(value)))
            .expect_err("invalid timeout value must be rejected");
        let message = err.to_string();
        assert!(
            matches!(err, CueEngineError::Configuration { .. }),
            "unexpected error kind for {value:?}: {message}"
        );
        assert!(message.contains(EVAL_TIMEOUT_ENV_VAR), "{message}");
        assert!(message.contains(value), "{message}");
        assert!(
            message.contains("positive whole number of seconds"),
            "{message}"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_module_eval_timeout_rejects_non_utf8_value() {
    use std::os::unix::ffi::OsStrExt;

    let err = super::parse_module_eval_timeout(Some(OsStr::from_bytes(&[0x31, 0xff])))
        .expect_err("non-UTF-8 timeout value must be rejected");
    assert!(matches!(err, CueEngineError::Configuration { .. }));
    assert!(err.to_string().contains(EVAL_TIMEOUT_ENV_VAR));
}

#[test]
fn test_module_eval_timeout_error_names_env_var() {
    let (_tx, rx) = std::sync::mpsc::sync_channel::<Result<ModuleResult>>(1);
    let err = super::receive_module_eval_result(
        &rx,
        &super::ModuleEvalWait {
            module_root: Path::new("/tmp/module"),
            package_name: "cuenv",
            timeout: Duration::from_millis(1),
        },
    )
    .expect_err("an unanswered evaluation must time out");
    let message = err.to_string();
    assert!(message.contains("CUE evaluation timed out"), "{message}");
    assert!(message.contains("/tmp/module"), "{message}");
    assert!(message.contains(EVAL_TIMEOUT_ENV_VAR), "{message}");
}
