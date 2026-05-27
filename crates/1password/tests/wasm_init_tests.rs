//! WASM initialization integration tests
//!
//! These tests verify that the 1Password WASM SDK can be loaded and the Extism
//! plugin can be initialized. This catches platform/runtime compatibility issues
//! that would otherwise only appear at secret resolution time.
//!
//! The WASM is automatically downloaded if not present in the cache.

use cuenv_1password::secrets::{core, wasm};
use std::error::Error;
use std::path::PathBuf;
use std::sync::OnceLock;

/// 1Password WASM SDK URL (pinned to v0.3.1)
const ONEPASSWORD_WASM_URL: &str =
    "https://github.com/1Password/onepassword-sdk-go/raw/refs/tags/v0.3.1/internal/wasm/core.wasm";

/// Minimum expected size for the 1Password WASM file (5 MB)
const MIN_WASM_SIZE: u64 = 5_000_000;
static RUSTLS_PROVIDER_INSTALLED: OnceLock<()> = OnceLock::new();

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn ensure_rustls_crypto_provider() {
    RUSTLS_PROVIDER_INSTALLED.get_or_init(|| {
        let provider_installed = rustls::crypto::CryptoProvider::get_default().is_some()
            || rustls::crypto::ring::default_provider()
                .install_default()
                .is_ok();

        assert!(
            provider_installed,
            "Failed to install rustls crypto provider"
        );
    });
}

/// Ensure WASM is available, downloading if necessary.
/// Uses atomic file operations to handle concurrent test execution safely.
fn ensure_wasm_available() -> TestResult<PathBuf> {
    let path = wasm::onepassword_wasm_path()?;

    // Check if file already exists with valid size (another test may have downloaded it)
    if let Ok(metadata) = std::fs::metadata(&path)
        && metadata.len() >= MIN_WASM_SIZE
    {
        return Ok(path);
    }

    // Create parent directory
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Use a temp file with process ID to avoid conflicts between parallel tests
    let temp_path = path.with_extension(format!("wasm.tmp.{}", std::process::id()));

    // Download using reqwest blocking client
    ensure_rustls_crypto_provider();
    let response = reqwest::blocking::get(ONEPASSWORD_WASM_URL)?;
    assert!(
        response.status().is_success(),
        "Failed to download WASM: HTTP {}",
        response.status()
    );

    let bytes = response.bytes()?;
    assert!(
        bytes.len() as u64 >= MIN_WASM_SIZE,
        "Downloaded WASM too small: {} bytes",
        bytes.len()
    );

    // Write to temp file first
    std::fs::write(&temp_path, &bytes)?;

    // Atomically rename to final path (handles concurrent downloads safely)
    // If another process already created the file, this may fail on some platforms,
    // but the file should still be valid from the other process
    if std::fs::rename(&temp_path, &path).is_err() {
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
    }

    // Verify final file exists and has valid size
    let final_metadata = std::fs::metadata(&path)?;
    assert!(
        final_metadata.len() >= MIN_WASM_SIZE,
        "Final WASM file too small: {} bytes",
        final_metadata.len()
    );

    Ok(path)
}

/// Test that the WASM file can be loaded and the Extism plugin initializes
///
/// This is the critical test - it verifies that the Extism runtime can load
/// the 1Password WASM SDK on this platform with the required host functions.
/// This is where CI failures occur if there's a platform compatibility issue.
///
/// Note: Scopes HOME for wasmtime's bytecode cache.
/// In Nix sandbox, HOME=/homeless-shelter which is unwritable.
///
#[test]
fn test_wasm_loads_and_plugin_initializes() -> TestResult {
    let path = ensure_wasm_available()?;

    let temp_home = tempfile::tempdir()?;
    temp_env::with_var("HOME", Some(temp_home.path()), || -> TestResult {
        // Load WASM bytes
        let bytes = std::fs::read(&path)?;

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
        let plugin = extism::Plugin::new(&manifest, host_functions, true)?;

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
        Ok(())
    })?;

    Ok(())
}

/// Test that WASM size is reasonable (catches incomplete downloads)
#[test]
fn test_wasm_file_size() -> TestResult {
    let path = ensure_wasm_available()?;
    let metadata = std::fs::metadata(&path)?;

    // The WASM file should be around 8-10 MB
    assert!(
        (5 * 1024 * 1024..20 * 1024 * 1024).contains(&metadata.len()),
        "WASM file should be 5-20 MB (got {} bytes)",
        metadata.len()
    );

    Ok(())
}

/// Test using `SharedCore` directly (same code path as production)
///
/// Note: Scopes HOME and ONEPASSWORD_WASM_PATH env vars.
/// HOME is changed for wasmtime's bytecode cache (in Nix sandbox, HOME=/homeless-shelter).
/// ONEPASSWORD_WASM_PATH ensures SharedCore finds the WASM after HOME is changed.
#[test]
fn test_shared_core_initializes() -> TestResult {
    let wasm_path = ensure_wasm_available()?;

    let temp_home = tempfile::tempdir()?;
    temp_env::with_vars(
        [
            ("ONEPASSWORD_WASM_PATH", Some(wasm_path.as_os_str())),
            ("HOME", Some(temp_home.path().as_os_str())),
        ],
        || -> TestResult {
            // This tests the full SharedCore initialization path
            let core_mutex = core::SharedCore::get_or_init()?;

            // Verify we can acquire the lock
            let guard = core_mutex
                .lock()
                .map_err(|_| std::io::Error::other("SharedCore lock should not be poisoned"))?;
            assert!(guard.is_some(), "SharedCore should be initialized");
            Ok(())
        },
    )?;

    Ok(())
}
