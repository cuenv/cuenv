//! Secrets provider setup commands

use crate::cli::{CliError, SecretsProvider};
use cuenv_core::http::ensure_rustls_crypto_provider;
use cuenv_events::println_redacted;

/// Default WASM URL for 1Password SDK v0.3.1
const ONEPASSWORD_WASM_URL: &str =
    "https://github.com/1Password/onepassword-sdk-go/raw/refs/tags/v0.3.1/internal/wasm/core.wasm";
const BYTES_PER_MEBIBYTE: usize = 1024 * 1024;

/// Execute the secrets setup command.
///
/// # Errors
///
/// Returns an error if the WASM file cannot be downloaded or saved.
pub fn execute_secrets_setup(
    provider: SecretsProvider,
    wasm_url: Option<&str>,
) -> Result<(), CliError> {
    match provider {
        SecretsProvider::Onepassword => setup_onepassword(wasm_url),
        SecretsProvider::Infisical => setup_infisical(),
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
        println_redacted(&format!(
            "1Password WASM SDK already downloaded at: {}",
            wasm_path.display()
        ));
        println_redacted("To re-download, delete the file and run this command again.");
        return Ok(());
    }

    println_redacted("Downloading 1Password WASM SDK...");
    println_redacted(&format!("Source: {url}"));

    // Download the WASM file
    ensure_rustls_crypto_provider();
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

    let size_mb = format_download_size_mb(bytes.len());
    println_redacted(&format!(
        "Downloaded {size_mb} MB to: {}",
        wasm_path.display()
    ));
    println_redacted("");
    println_redacted("1Password HTTP mode is now enabled!");
    println_redacted("Set OP_SERVICE_ACCOUNT_TOKEN to use HTTP mode instead of CLI.");

    Ok(())
}

/// Set up Infisical by validating required authentication environment variables.
fn setup_infisical() -> Result<(), CliError> {
    let has_client_id = has_env("INFISICAL_CLIENT_ID");
    let has_client_secret = has_env("INFISICAL_CLIENT_SECRET");
    let has_token = has_env("INFISICAL_TOKEN");

    match (has_client_id, has_client_secret, has_token) {
        (true, true, _) => {
            println_redacted("Infisical Universal Auth environment detected.");
            println_redacted("cuenv will use INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET.");
            Ok(())
        }
        (false, false, true) => {
            println_redacted("Infisical token environment detected.");
            println_redacted("cuenv will use INFISICAL_TOKEN.");
            Ok(())
        }
        (true, false, _) | (false, true, _) => Err(CliError::config(
            "INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET must be set together.",
        )),
        (false, false, false) => Err(CliError::config(
            "Set INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET, or set INFISICAL_TOKEN.",
        )),
    }
}

fn has_env(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

fn format_download_size_mb(byte_count: usize) -> String {
    let centi_mb = ((byte_count as u128) * 100 + (BYTES_PER_MEBIBYTE as u128 / 2))
        / BYTES_PER_MEBIBYTE as u128;

    format!("{}.{:02}", centi_mb / 100, centi_mb % 100)
}

#[cfg(test)]
mod tests {
    use super::{BYTES_PER_MEBIBYTE, format_download_size_mb};

    #[test]
    fn format_download_size_mb_keeps_two_decimal_places() {
        assert_eq!(format_download_size_mb(0), "0.00");
        assert_eq!(format_download_size_mb(1), "0.00");
        assert_eq!(format_download_size_mb(BYTES_PER_MEBIBYTE), "1.00");
        assert_eq!(format_download_size_mb(BYTES_PER_MEBIBYTE * 3 / 2), "1.50");
        assert_eq!(format_download_size_mb(BYTES_PER_MEBIBYTE - 1), "1.00");
    }
}
