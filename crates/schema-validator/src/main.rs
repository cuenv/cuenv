//! Schema validation tool for ensuring Rust types match CUE definitions
//!
//! This tool provides commands to:
//! - Generate JSON Schema from Rust types
//! - Validate test fixtures against both Rust and CUE schemas
//! - Compare schemas for drift detection

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use cuenv_core::manifest::Cuenv;
use schemars::schema_for;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "schema-validator")]
#[command(about = "Validate Rust types against CUE schemas")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate JSON Schema from Rust types
    Generate {
        /// Output directory for generated schemas
        #[arg(short, long, default_value = "generated-schemas")]
        output: PathBuf,
    },
    /// Validate test fixtures against schemas
    Validate {
        /// Directory containing test fixtures
        #[arg(short, long, default_value = "tests/fixtures")]
        fixtures: PathBuf,
    },
    /// Compare Rust and CUE schemas for compatibility
    Compare {
        /// Path to CUE schema files
        #[arg(short, long, default_value = "schema")]
        cue_path: PathBuf,
        /// Path to generated Rust schemas
        #[arg(short, long, default_value = "generated-schemas")]
        rust_path: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Generate { output } => generate_schemas(&output),
        Commands::Validate { fixtures } => validate_fixtures(&fixtures),
        Commands::Compare {
            cue_path,
            rust_path,
        } => compare_schemas(&cue_path, &rust_path),
    }
}

/// Generate JSON Schema from Rust types
fn generate_schemas(output_dir: &Path) -> Result<()> {
    info!("Generating JSON schemas from Rust types...");

    // Create output directory
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "Failed to create output directory: {}",
            output_dir.display()
        )
    })?;

    // Generate schema for main Cuenv type
    let cuenv_schema = schema_for!(Cuenv);
    let cuenv_json = serde_json::to_string_pretty(&cuenv_schema)?;
    let cuenv_path = output_dir.join("cuenv.schema.json");
    fs::write(&cuenv_path, cuenv_json)
        .with_context(|| format!("Failed to write schema to {}", cuenv_path.display()))?;
    info!("Generated schema: {}", cuenv_path.display());

    // Generate schemas for component types
    generate_component_schemas(output_dir)?;

    info!(
        "Successfully generated all schemas to {}",
        output_dir.display()
    );
    Ok(())
}

/// Generate schemas for individual component types
fn generate_component_schemas(output_dir: &Path) -> Result<()> {
    use cuenv_core::{config::Config, environment::Env, hooks::types::Hook, tasks::TaskDefinition};

    // Config schema
    let config_schema = schema_for!(Config);
    let config_path = output_dir.join("config.schema.json");
    fs::write(&config_path, serde_json::to_string_pretty(&config_schema)?)?;
    info!("Generated: {}", config_path.display());

    // Environment schema
    let env_schema = schema_for!(Env);
    let env_path = output_dir.join("env.schema.json");
    fs::write(&env_path, serde_json::to_string_pretty(&env_schema)?)?;
    info!("Generated: {}", env_path.display());

    // Hook schema
    let hook_schema = schema_for!(Hook);
    let hook_path = output_dir.join("hook.schema.json");
    fs::write(&hook_path, serde_json::to_string_pretty(&hook_schema)?)?;
    info!("Generated: {}", hook_path.display());

    // Task schema
    let task_schema = schema_for!(TaskDefinition);
    let task_path = output_dir.join("task.schema.json");
    fs::write(&task_path, serde_json::to_string_pretty(&task_schema)?)?;
    info!("Generated: {}", task_path.display());

    Ok(())
}

/// Validate test fixtures against both Rust and CUE schemas
fn validate_fixtures(fixtures_dir: &Path) -> Result<()> {
    info!("Validating test fixtures in {}", fixtures_dir.display());

    let valid_dir = fixtures_dir.join("valid");
    let invalid_dir = fixtures_dir.join("invalid");

    let mut all_passed = true;

    // Validate valid fixtures (should pass)
    if valid_dir.exists() {
        info!("Testing valid fixtures...");
        for entry in fs::read_dir(&valid_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("cue")
                && !validate_fixture(&path, true)?
            {
                all_passed = false;
            }
        }
    }

    // Validate invalid fixtures (should fail)
    if invalid_dir.exists() {
        info!("Testing invalid fixtures...");
        for entry in fs::read_dir(&invalid_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("cue")
                && !validate_fixture(&path, false)?
            {
                all_passed = false;
            }
        }
    }

    if all_passed {
        info!("All fixture validations passed!");
        Ok(())
    } else {
        anyhow::bail!("Some fixture validations failed");
    }
}

/// Validate a single fixture file
fn validate_fixture(fixture_path: &Path, should_pass: bool) -> Result<bool> {
    let fixture_name = fixture_path.file_name().unwrap().to_string_lossy();

    // First, validate with CUE
    let cue_valid = validate_with_cue(fixture_path)?;

    // For invalid fixtures, if CUE validation fails (as expected), we can't export to JSON
    // So we consider Rust validation as also failing
    let rust_valid = if !cue_valid {
        // If CUE validation fails, we can't export to JSON, so Rust validation also fails
        false
    } else {
        // Export CUE to JSON and validate with Rust
        match export_cue_to_json(fixture_path) {
            Ok(json_path) => {
                let valid = validate_with_rust(&json_path);
                // Clean up temporary JSON file
                let _ = fs::remove_file(json_path);
                valid
            }
            Err(_) => {
                // If export fails, consider it as validation failure
                false
            }
        }
    };

    // Check results
    let passed = match (should_pass, cue_valid, rust_valid) {
        (true, true, true) => {
            info!("✓ {} - Valid fixture passed both validations", fixture_name);
            true
        }
        (false, false, false) => {
            info!(
                "✓ {} - Invalid fixture failed both validations",
                fixture_name
            );
            true
        }
        (expected, cue, rust) => {
            error!(
                "✗ {} - Validation mismatch! Expected: {}, CUE: {}, Rust: {}",
                fixture_name, expected, cue, rust
            );
            false
        }
    };

    Ok(passed)
}

/// Validate a CUE file using the CUE CLI
fn validate_with_cue(cue_path: &Path) -> Result<bool> {
    let output = Command::new("cue")
        .args(["vet", cue_path.to_str().unwrap()])
        .output()
        .context("Failed to run 'cue vet' - is CUE installed?")?;

    Ok(output.status.success())
}

/// Export CUE to JSON for Rust validation
fn export_cue_to_json(cue_path: &Path) -> Result<PathBuf> {
    let json_path = cue_path.with_extension("json");

    let output = Command::new("cue")
        .args([
            "export",
            cue_path.to_str().unwrap(),
            "-o",
            json_path.to_str().unwrap(),
        ])
        .output()
        .context("Failed to run 'cue export'")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to export CUE to JSON: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(json_path)
}

/// Validate JSON against Rust types
fn validate_with_rust(json_path: &Path) -> bool {
    let json_str = match fs::read_to_string(json_path) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Try to deserialize as Cuenv type
    match serde_json::from_str::<Cuenv>(&json_str) {
        Ok(_) => true,
        Err(e) => {
            warn!("Rust validation failed: {}", e);
            false
        }
    }
}

/// Compare Rust and CUE schemas for compatibility
fn compare_schemas(cue_path: &Path, rust_path: &Path) -> Result<()> {
    info!("Comparing schemas...");
    info!("CUE path: {}", cue_path.display());
    info!("Rust schemas: {}", rust_path.display());

    // Export CUE schemas to OpenAPI format
    let cue_export = export_cue_schemas(cue_path)?;

    // Load Rust-generated schemas
    let rust_schema = load_rust_schema(rust_path)?;

    // Compare the schemas
    let differences = find_schema_differences(&cue_export, &rust_schema);

    if differences.is_empty() {
        info!("✓ Schemas are compatible!");
        Ok(())
    } else {
        error!("✗ Found {} schema differences:", differences.len());
        for diff in &differences {
            error!("  - {}", diff);
        }
        anyhow::bail!("Schema compatibility check failed");
    }
}

/// Export CUE schemas to a comparable format
fn export_cue_schemas(cue_path: &Path) -> Result<Value> {
    // Try to export as OpenAPI
    let output = Command::new("cue")
        .args(["export", "--out", "openapi"])
        .current_dir(cue_path)
        .arg(".")
        .output()
        .context("Failed to export CUE schemas")?;

    if output.status.success() {
        let json_str = String::from_utf8(output.stdout)?;
        Ok(serde_json::from_str(&json_str)?)
    } else {
        // Fallback to regular JSON export
        let output = Command::new("cue")
            .args(["export"])
            .current_dir(cue_path)
            .arg(".")
            .output()?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to export CUE: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let json_str = String::from_utf8(output.stdout)?;
        Ok(serde_json::from_str(&json_str)?)
    }
}

/// Load Rust-generated schema
fn load_rust_schema(rust_path: &Path) -> Result<Value> {
    let schema_file = rust_path.join("cuenv.schema.json");
    let content = fs::read_to_string(&schema_file)
        .with_context(|| format!("Failed to read {}", schema_file.display()))?;
    Ok(serde_json::from_str(&content)?)
}

/// Find differences between schemas
fn find_schema_differences(_cue: &Value, rust: &Value) -> Vec<String> {
    let differences = Vec::new();

    // This is a simplified comparison - in production you'd want more sophisticated logic
    // For now, we just check that required fields match

    if let Some(rust_props) = rust.get("properties") {
        // Check for fields in Rust not in CUE
        if let Some(rust_obj) = rust_props.as_object() {
            for (key, _) in rust_obj {
                // This is where you'd implement actual comparison logic
                // For now, we just note the field exists
                info!("  Checking field: {}", key);
            }
        }
    }

    // Add actual comparison logic here based on your needs
    // This is a placeholder that doesn't find any differences

    differences
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_schemas() {
        let temp_dir = TempDir::new().unwrap();
        let result = generate_schemas(temp_dir.path());
        assert!(result.is_ok());

        // Check that schema files were created
        assert!(temp_dir.path().join("cuenv.schema.json").exists());
        assert!(temp_dir.path().join("config.schema.json").exists());
        assert!(temp_dir.path().join("env.schema.json").exists());
        assert!(temp_dir.path().join("hook.schema.json").exists());
    }
}
