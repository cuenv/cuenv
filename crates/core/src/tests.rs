use super::*;
use miette::SourceSpan;
use std::path::Path;

#[test]
fn test_error_configuration() {
    let err = Error::configuration("test message");
    assert_eq!(err.to_string(), "Configuration error: test message");

    if let Error::Configuration { message, .. } = err {
        assert_eq!(message, "test message");
    } else {
        panic!("Expected Configuration error");
    }
}

#[test]
fn test_error_configuration_with_source() {
    let src = "test source code";
    let span = SourceSpan::from(0..4);
    let err = Error::configuration_with_source("config error", src, Some(span));

    if let Error::Configuration {
        src: source,
        span: s,
        message,
    } = err
    {
        assert_eq!(source, "test source code");
        assert_eq!(s, Some(SourceSpan::from(0..4)));
        assert_eq!(message, "config error");
    } else {
        panic!("Expected Configuration error");
    }
}

#[test]
fn test_error_ffi() {
    let err = Error::ffi("test_function", "FFI failed");
    assert_eq!(
        err.to_string(),
        "FFI operation failed in test_function: FFI failed"
    );

    if let Error::Ffi {
        function,
        message,
        help,
    } = err
    {
        assert_eq!(function, "test_function");
        assert_eq!(message, "FFI failed");
        assert!(help.is_none());
    } else {
        panic!("Expected Ffi error");
    }
}

#[test]
fn test_error_ffi_with_help() {
    let err = Error::ffi_with_help("test_func", "error msg", "try this instead");

    if let Error::Ffi {
        function,
        message,
        help,
    } = err
    {
        assert_eq!(function, "test_func");
        assert_eq!(message, "error msg");
        assert_eq!(help, Some("try this instead".to_string()));
    } else {
        panic!("Expected Ffi error");
    }
}

#[test]
fn test_error_cue_parse() {
    let path = Path::new("/test/path.cue");
    let err = Error::cue_parse(path, "parsing failed");
    assert_eq!(err.to_string(), "CUE parsing failed: parsing failed");

    if let Error::CueParse {
        path: p, message, ..
    } = err
    {
        assert_eq!(p.as_ref(), Path::new("/test/path.cue"));
        assert_eq!(message, "parsing failed");
    } else {
        panic!("Expected CueParse error");
    }
}

#[test]
fn test_error_cue_parse_with_source() {
    let path = Path::new("/test/file.cue");
    let src = "package test";
    let span = SourceSpan::from(0..7);
    let suggestions = vec!["Check syntax".to_string(), "Verify imports".to_string()];

    let err = Error::cue_parse_with_source(
        path,
        "parse error",
        src,
        Some(span),
        Some(suggestions.clone()),
    );

    if let Error::CueParse {
        path: p,
        src: source,
        span: s,
        message,
        suggestions: sugg,
    } = err
    {
        assert_eq!(p.as_ref(), Path::new("/test/file.cue"));
        assert_eq!(source, Some("package test".to_string()));
        assert_eq!(s, Some(SourceSpan::from(0..7)));
        assert_eq!(message, "parse error");
        assert_eq!(sugg, Some(suggestions));
    } else {
        panic!("Expected CueParse error");
    }
}

#[test]
fn test_error_validation() {
    let err = Error::validation("validation failed");
    assert_eq!(err.to_string(), "Validation failed: validation failed");

    if let Error::Validation {
        message, related, ..
    } = err
    {
        assert_eq!(message, "validation failed");
        assert!(related.is_empty());
    } else {
        panic!("Expected Validation error");
    }
}

#[test]
fn test_error_validation_with_source() {
    let src = "test validation source";
    let span = SourceSpan::from(5..15);
    let err = Error::validation_with_source("validation error", src, Some(span));

    if let Error::Validation {
        src: source,
        span: s,
        message,
        ..
    } = err
    {
        assert_eq!(source, Some("test validation source".to_string()));
        assert_eq!(s, Some(SourceSpan::from(5..15)));
        assert_eq!(message, "validation error");
    } else {
        panic!("Expected Validation error");
    }
}

#[test]
fn test_error_timeout() {
    let err = Error::Timeout { seconds: 30 };
    assert_eq!(err.to_string(), "Operation timed out after 30 seconds");
}

#[test]
fn test_error_from_io_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    let err: Error = io_err.into();

    if let Error::Io { operation, .. } = err {
        assert_eq!(operation, "unknown (unmapped error conversion)");
    } else {
        panic!("Expected Io error");
    }
}

#[test]
fn test_error_from_utf8_error() {
    let bytes = vec![0xFF, 0xFE];
    let utf8_err = std::str::from_utf8(&bytes).unwrap_err();
    let err: Error = utf8_err.into();

    assert!(matches!(err, Error::Utf8 { .. }));
}

#[test]
fn test_limits_default() {
    let limits = Limits::default();
    assert_eq!(limits.max_path_length, 4096);
    assert_eq!(limits.max_package_name_length, 256);
    assert_eq!(limits.max_output_size, 100 * 1024 * 1024);
}

#[test]
fn test_result_type_alias() {
    let ok_result: Result<i32> = Ok(42);
    assert!(ok_result.is_ok());
    if let Ok(value) = ok_result {
        assert_eq!(value, 42);
    }

    let err_result: Result<i32> = Err(Error::configuration("test"));
    assert!(err_result.is_err());
}

#[test]
fn test_error_display() {
    let errors = vec![
        (Error::configuration("test"), "Configuration error: test"),
        (
            Error::ffi("func", "msg"),
            "FFI operation failed in func: msg",
        ),
        (
            Error::cue_parse(Path::new("/test"), "msg"),
            "CUE parsing failed: msg",
        ),
        (Error::validation("msg"), "Validation failed: msg"),
        (
            Error::Timeout { seconds: 10 },
            "Operation timed out after 10 seconds",
        ),
    ];

    for (error, expected) in errors {
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn test_error_diagnostic_codes() {
    use miette::Diagnostic;

    let config_err = Error::configuration("test");
    assert_eq!(
        config_err.code().unwrap().to_string(),
        "cuenv::config::invalid"
    );

    let ffi_err = Error::ffi("func", "msg");
    assert_eq!(ffi_err.code().unwrap().to_string(), "cuenv::ffi::error");

    let cue_err = Error::cue_parse(Path::new("/test"), "msg");
    assert_eq!(
        cue_err.code().unwrap().to_string(),
        "cuenv::cue::parse_error"
    );

    let validation_err = Error::validation("msg");
    assert_eq!(
        validation_err.code().unwrap().to_string(),
        "cuenv::validation::failed"
    );

    let timeout_err = Error::Timeout { seconds: 5 };
    assert_eq!(timeout_err.code().unwrap().to_string(), "cuenv::timeout");
}

#[test]
fn test_package_dir_validation() {
    // Current directory should be valid
    let result = PackageDir::try_from(Path::new("."));
    assert!(result.is_ok(), "Current directory should be valid");

    // Get methods should work
    let pkg_dir = result.unwrap();
    assert_eq!(pkg_dir.as_path(), Path::new("."));
    assert_eq!(pkg_dir.as_ref(), Path::new("."));
    assert_eq!(pkg_dir.into_path_buf(), PathBuf::from("."));

    // Non-existent directory should fail with NotFound
    let result = PackageDir::try_from(Path::new("/path/does/not/exist"));
    assert!(result.is_err());
    match result.unwrap_err() {
        PackageDirError::NotFound(_) => {} // Expected
        other => panic!("Expected NotFound error, got: {:?}", other),
    }

    // Path to a file should fail with NotADirectory
    // Create a temporary file
    let temp_path = std::env::temp_dir().join("cuenv_test_file");
    let file = std::fs::File::create(&temp_path).unwrap();
    drop(file);

    let result = PackageDir::try_from(temp_path.as_path());
    assert!(result.is_err());
    match result.unwrap_err() {
        PackageDirError::NotADirectory(_) => {} // Expected
        other => panic!("Expected NotADirectory error, got: {:?}", other),
    }

    // Clean up
    std::fs::remove_file(temp_path).ok();
}

#[test]
fn test_package_name_validation() {
    // Valid package names
    let max_len_string = "a".repeat(64);
    let valid_names = vec![
        "my-package",
        "package_123",
        "a",        // Single character
        "A",        // Uppercase
        "0package", // Starts with number
        "package-with-hyphens",
        "package_with_underscores",
        max_len_string.as_str(), // Max length
    ];

    for name in valid_names {
        let result = PackageName::try_from(name);
        assert!(result.is_ok(), "'{}' should be valid", name);

        // Test the String variant too
        let result = PackageName::try_from(name.to_string());
        assert!(result.is_ok(), "'{}' as String should be valid", name);

        // Verify methods work correctly
        let pkg_name = result.unwrap();
        assert_eq!(pkg_name.as_str(), name);
        assert_eq!(pkg_name.as_ref(), name);
        assert_eq!(pkg_name.to_string(), name);
        assert_eq!(pkg_name.into_string(), name.to_string());
    }

    // Invalid package names
    let too_long_string = "a".repeat(65);
    let invalid_names = vec![
        "",                       // Empty
        "-invalid",               // Starts with hyphen
        "_invalid",               // Starts with underscore
        "invalid.name",           // Contains dot
        "invalid/name",           // Contains slash
        "invalid:name",           // Contains colon
        too_long_string.as_str(), // Too long
        "invalid@name",           // Contains @
        "invalid#name",           // Contains #
        "invalid name",           // Contains space
    ];

    for name in invalid_names {
        let result = PackageName::try_from(name);
        assert!(result.is_err(), "'{}' should be invalid", name);

        // Verify error type is correct
        assert!(matches!(result.unwrap_err(), PackageNameError::Invalid(_)));
    }
}
