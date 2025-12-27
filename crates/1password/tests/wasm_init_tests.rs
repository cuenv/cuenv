//! WASM initialization integration tests
//!
//! These tests verify that the 1Password WASM SDK can be loaded and the Extism
//! plugin can be initialized. This catches platform/runtime compatibility issues
//! that would otherwise only appear at secret resolution time.
//!
//! In CI (Nix builds), the WASM is provided via `ONEPASSWORD_WASM_PATH` env var.
//! For local development, run `cuenv secrets setup onepassword` to download it.
//!
//! Tests are skipped if WASM is not available (common in CI without 1Password setup).

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::expect_used)]

use cuenv_1password::secrets::{core, wasm};
use std::path::PathBuf;

/// Ensure WASM is available (from env var or cache).
/// Returns None if WASM is not available, allowing tests to skip gracefully.
fn ensure_wasm_available() -> Option<PathBuf> {
    let path = wasm::onepassword_wasm_path().ok()?;

    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Test that the WASM file can be loaded and the Extism plugin initializes
///
/// This is the critical test - it verifies that the Extism runtime can load
/// the 1Password WASM SDK on this platform with the required host functions.
/// This is where CI failures occur if there's a platform compatibility issue.
///
/// Note: Uses unsafe to set HOME env var for wasmtime's bytecode cache.
/// In Nix sandbox, HOME=/homeless-shelter which is unwritable.
#[test]
#[allow(unsafe_code)]
fn test_wasm_loads_and_plugin_initializes() {
    let Some(path) = ensure_wasm_available() else {
        eprintln!("Skipping test: 1Password WASM not available");
        return;
    };

    // Set HOME to a temp directory for wasmtime's bytecode cache.
    let temp_home = std::env::temp_dir().join("cuenv-wasm-test-home");
    std::fs::create_dir_all(&temp_home).expect("Should create temp home");
    // SAFETY: This test runs in isolation and no other threads are reading
    // environment variables concurrently during this initialization.
    unsafe { std::env::set_var("HOME", &temp_home) };

    // Load WASM bytes
    let bytes = std::fs::read(&path).expect("Should read WASM file");

    // Sanity check - the WASM should be several MB
    let len = bytes.len();
    assert!(
        len > 1_000_000,
        "WASM should be > 1MB (got {len} bytes) - file may be corrupt or incomplete"
    );

    // Create Extism manifest with allowed hosts (same as core.rs)
    let manifest = extism::Manifest::new([extism::Wasm::data(bytes)]).with_allowed_hosts(
        ["*.1password.com", "*.1password.ca", "*.1password.eu"]
            .into_iter()
            .map(String::from),
    );

    // Get the host functions required by the 1Password WASM SDK
    let host_functions = core::create_host_functions();

    // Initialize plugin with host functions - THIS IS THE CRITICAL TEST
    // This is where failures occur if the WASM is incompatible with the platform
    let plugin = extism::Plugin::new(&manifest, host_functions, true)
        .expect("Extism plugin initialization should succeed");

    // Verify the expected function exports exist
    // These are the functions called by the SharedCore
    assert!(
        plugin.function_exists("init_client"),
        "Plugin should export init_client function"
    );
    assert!(
        plugin.function_exists("invoke"),
        "Plugin should export invoke function"
    );
    assert!(
        plugin.function_exists("release_client"),
        "Plugin should export release_client function"
    );
}

/// Test that WASM size is reasonable (catches incomplete downloads)
#[test]
fn test_wasm_file_size() {
    let Some(path) = ensure_wasm_available() else {
        eprintln!("Skipping test: 1Password WASM not available");
        return;
    };
    let metadata = std::fs::metadata(&path).expect("Should get file metadata");

    // The WASM file should be around 8-10 MB
    // Precision loss is acceptable for display purposes
    #[expect(clippy::cast_precision_loss)]
    let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);

    assert!(
        size_mb > 5.0 && size_mb < 20.0,
        "WASM file should be 5-20 MB (got {size_mb:.2} MB)"
    );
}

/// Test using `SharedCore` directly (same code path as production)
///
/// Note: Uses unsafe to set HOME env var for wasmtime's bytecode cache.
/// In Nix sandbox, HOME=/homeless-shelter which is unwritable.
#[test]
#[allow(unsafe_code, clippy::significant_drop_tightening)]
fn test_shared_core_initializes() {
    let Some(_path) = ensure_wasm_available() else {
        eprintln!("Skipping test: 1Password WASM not available");
        return;
    };

    // Set HOME to a temp directory for wasmtime's bytecode cache.
    let temp_home = std::env::temp_dir().join("cuenv-wasm-test-home");
    std::fs::create_dir_all(&temp_home).expect("Should create temp home");
    // SAFETY: This test runs in isolation and no other threads are reading
    // environment variables concurrently during this initialization.
    unsafe { std::env::set_var("HOME", &temp_home) };

    // This tests the full SharedCore initialization path
    let core_mutex =
        core::SharedCore::get_or_init().expect("SharedCore should initialize successfully");

    // Verify we can acquire the lock
    let guard = core_mutex.lock().expect("Should acquire lock");
    assert!(guard.is_some(), "SharedCore should be initialized");
}
