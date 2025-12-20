//! Secrets provider setup commands

use crate::cli::{CliError, SecretsProvider};

/// Default WASM URL for 1Password SDK v0.3.1
const ONEPASSWORD_WASM_URL: &str =
    "https://github.com/1Password/onepassword-sdk-go/raw/refs/tags/v0.3.1/internal/wasm/core.wasm";

/// Execute the secrets setup command
pub fn execute_secrets_setup(
    provider: SecretsProvider,
    wasm_url: Option<&str>,
) -> Result<(), CliError> {
    match provider {
        SecretsProvider::Onepassword => setup_onepassword(wasm_url),
    }
}

/// Set up 1Password by downloading the WASM SDK
fn setup_onepassword(wasm_url: Option<&str>) -> Result<(), CliError> {
    let url = wasm_url.unwrap_or(ONEPASSWORD_WASM_URL);

    // Get cache directory
    let cache_dir = dirs::cache_dir()
        .ok_or_else(|| CliError::config("Could not determine cache directory"))?
        .join("cuenv")
        .join("wasm");

    // Create cache directory if it doesn't exist
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| CliError::config(format!("Failed to create cache directory: {e}")))?;

    let wasm_path = cache_dir.join("onepassword-core.wasm");

    // Check if already downloaded
    if wasm_path.exists() {
        println!(
            "1Password WASM SDK already downloaded at: {}",
            wasm_path.display()
        );
        println!("To re-download, delete the file and run this command again.");
        return Ok(());
    }

    println!("Downloading 1Password WASM SDK...");
    println!("Source: {url}");

    // Download the WASM file
    let response = reqwest::blocking::get(url)
        .map_err(|e| CliError::config(format!("Failed to download WASM: {e}")))?;

    if !response.status().is_success() {
        return Err(CliError::config(format!(
            "Failed to download WASM: HTTP {}",
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .map_err(|e| CliError::config(format!("Failed to read response: {e}")))?;

    // Write to cache
    std::fs::write(&wasm_path, &bytes)
        .map_err(|e| CliError::config(format!("Failed to write WASM file: {e}")))?;

    #[allow(clippy::cast_precision_loss)] // Precision loss acceptable for display
    let size_mb = bytes.len() as f64 / (1024.0 * 1024.0);
    println!("Downloaded {size_mb:.2} MB to: {}", wasm_path.display());
    println!();
    println!("1Password HTTP mode is now enabled!");
    println!("Set OP_SERVICE_ACCOUNT_TOKEN to use HTTP mode instead of CLI.");

    Ok(())
}
