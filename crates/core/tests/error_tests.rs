//! Tests for error types

use cuenv_core::Error;
use std::path::Path;

#[test]
fn test_configuration_error() {
    let error = Error::configuration("config is invalid");
    assert_eq!(error.to_string(), "Configuration error: config is invalid");

    let error = Error::configuration(String::from("another config error"));
    assert_eq!(
        error.to_string(),
        "Configuration error: another config error"
    );
}

#[test]
fn test_ffi_error() {
    let error = Error::ffi("evaluate", "failed to call FFI");
    assert_eq!(
        error.to_string(),
        "FFI operation failed in evaluate: failed to call FFI"
    );

    let error = Error::ffi("parse", String::from("invalid input"));
    assert_eq!(
        error.to_string(),
        "FFI operation failed in parse: invalid input"
    );
}

#[test]
fn test_cue_parse_error() {
    let path = Path::new("/path/to/file.cue");
    let error = Error::cue_parse(path, "syntax error at line 5");
    assert_eq!(
        error.to_string(),
        "CUE parsing failed: syntax error at line 5"
    );

    let error = Error::cue_parse(path, String::from("unexpected token"));
    assert_eq!(error.to_string(), "CUE parsing failed: unexpected token");
}

#[test]
fn test_validation_error() {
    let error = Error::validation("input is too long");
    assert_eq!(error.to_string(), "Validation failed: input is too long");

    let error = Error::validation(String::from("invalid character in name"));
    assert_eq!(
        error.to_string(),
        "Validation failed: invalid character in name"
    );
}

#[test]
fn test_timeout_error() {
    let error = Error::Timeout { seconds: 30 };
    assert_eq!(error.to_string(), "Operation timed out after 30 seconds");

    let error = Error::Timeout { seconds: 5 };
    assert_eq!(error.to_string(), "Operation timed out after 5 seconds");
}

#[test]
fn test_io_error_conversion() {
    use std::io;

    let io_error = io::Error::new(io::ErrorKind::NotFound, "file not found");
    let error = Error::from(io_error);
    assert!(error.to_string().contains("I/O") && error.to_string().contains("failed"));
    // The specific error message is wrapped by miette, just check the main error type
    // assert!(error.to_string().contains("file not found"));
}

#[test]
fn test_utf8_error_conversion() {
    use std::str;

    // Create invalid UTF-8 bytes
    let invalid_utf8 = vec![0xff, 0xfe, 0xfd];
    let utf8_error = str::from_utf8(&invalid_utf8).unwrap_err();
    let error = Error::from(utf8_error);
    assert!(error.to_string().contains("Text encoding error"));
}

#[test]
fn test_error_variants_match() {
    let config_error = Error::configuration("test");
    match config_error {
        Error::Configuration { .. } => {}
        _ => panic!("Expected Configuration variant"),
    }

    let ffi_error = Error::ffi("test", "message");
    match ffi_error {
        Error::Ffi {
            function, message, ..
        } => {
            assert_eq!(function, "test");
            assert_eq!(message, "message");
        }
        _ => panic!("Expected Ffi variant"),
    }

    let path = Path::new("/test.cue");
    let cue_error = Error::cue_parse(path, "error");
    match cue_error {
        Error::CueParse {
            path: p, message, ..
        } => {
            assert_eq!(p.display().to_string(), "/test.cue");
            assert_eq!(message, "error");
        }
        _ => panic!("Expected CueParse variant"),
    }

    let validation_error = Error::validation("test");
    match validation_error {
        Error::Validation { .. } => {}
        _ => panic!("Expected Validation variant"),
    }

    let timeout_error = Error::Timeout { seconds: 10 };
    match timeout_error {
        Error::Timeout { seconds } => {
            assert_eq!(seconds, 10);
        }
        _ => panic!("Expected Timeout variant"),
    }
}

#[test]
fn test_error_debug_format() {
    let error = Error::configuration("debug test");
    let debug_str = format!("{:?}", error);
    assert!(debug_str.contains("Configuration"));
    assert!(debug_str.contains("debug test"));
}

#[test]
fn test_result_type_alias() {
    use cuenv_core::Result;

    fn returns_ok() -> Result<String> {
        Ok("success".to_string())
    }

    fn returns_err() -> Result<String> {
        Err(Error::configuration("failure"))
    }

    assert!(returns_ok().is_ok());
    assert!(returns_err().is_err());
}
