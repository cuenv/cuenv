//! Core types and utilities for cuenv

use std::path::Path;
use thiserror::Error;

/// Main error type for cuenv operations
#[derive(Error, Debug)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Configuration(String),
    
    #[error("FFI error in {function}: {message}")]
    Ffi {
        function: &'static str,
        message: String,
    },
    
    #[error("CUE parse error in {}: {message}", path.display())]
    CueParse {
        path: Box<Path>,
        message: String,
    },
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("UTF-8 conversion error: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    
    #[error("Timeout after {seconds} seconds")]
    Timeout { seconds: u64 },
    
    #[error("Validation error: {0}")]
    Validation(String),
}

impl Error {
    pub fn configuration(msg: impl Into<String>) -> Self {
        Error::Configuration(msg.into())
    }
    
    pub fn ffi(function: &'static str, message: impl Into<String>) -> Self {
        Error::Ffi {
            function,
            message: message.into(),
        }
    }
    
    pub fn cue_parse(path: &Path, message: impl Into<String>) -> Self {
        Error::CueParse {
            path: path.into(),
            message: message.into(),
        }
    }
    
    pub fn validation(msg: impl Into<String>) -> Self {
        Error::Validation(msg.into())
    }
}

/// Result type alias for cuenv operations
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration limits
pub struct Limits {
    pub max_path_length: usize,
    pub max_package_name_length: usize,
    pub max_output_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_path_length: 4096,
            max_package_name_length: 256,
            max_output_size: 100 * 1024 * 1024, // 100MB
        }
    }
}