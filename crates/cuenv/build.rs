//! Build script to generate llms-full.txt by concatenating llms.txt with CUE schemas.
//!
//! This script looks for llms.txt and schema/ in the workspace root.
//! When building from crates.io (where these files aren't included), it creates
//! a minimal placeholder file instead.

use std::error::Error;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use chrono::{SecondsFormat, Utc};

/// Returns the value of the environment variable or an error if it's not set.
fn env(key: &str) -> Result<std::ffi::OsString, Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed={}", key);
    std::env::var_os(key).ok_or_else(|| format!("Environment variable {key} not set").into())
}

/// Injects build metadata into the environment for use in the version command.
fn emit_build_metadata() -> Result<(), Box<dyn Error>> {
    let target = env("TARGET")?.to_string_lossy().into_owned();
    println!("cargo:rustc-env=CUENV_BUILD_TARGET={target}");

    // run "rustc --version" to get the Rust compiler version
    let rustc_version = env("RUSTC")
        .ok()
        .map(|rustc| rustc.to_string_lossy().into_owned())
        .and_then(|rustc| {
            Command::new(rustc)
                .arg("--version")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|version| version.trim().to_string())
                .filter(|version| !version.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CUENV_BUILD_RUSTC_VERSION={rustc_version}");

    // set the build date using SOURCE_DATE_EPOCH if available
    let build_date = env("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.to_string_lossy().parse::<i64>().ok())
        .and_then(|seconds| chrono::DateTime::<Utc>::from_timestamp(seconds, 0))
        .map_or_else(
            || Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            |datetime| datetime.to_rfc3339_opts(SecondsFormat::Secs, true),
        );
    println!("cargo:rustc-env=CUENV_BUILD_DATE={build_date}");

    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    emit_build_metadata()?;

    // Use CARGO_MANIFEST_DIR to get workspace root reliably
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_dir.parent().and_then(|p| p.parent());

    let out_dir = std::env::var("OUT_DIR")?;
    let output_path = format!("{out_dir}/llms-full.txt");

    // Try to find llms.txt and schema/ in workspace root
    let (llms_path, schema_dir) = workspace_root.map_or((None, None), |root| {
        let llms = root.join("llms.txt");
        let schema = root.join("schema");
        if llms.exists() && schema.exists() {
            println!("cargo:rerun-if-changed={}", llms.display());
            println!("cargo:rerun-if-changed={}", schema.display());
            (Some(llms), Some(schema))
        } else {
            (None, None)
        }
    });

    // If files exist, generate full llms-full.txt
    if let (Some(llms_path), Some(schema_dir)) = (llms_path, schema_dir) {
        let mut output = fs::read_to_string(&llms_path)?;

        output.push_str("\n## CUE Schema Reference\n");

        // Read all .cue files from schema/
        let mut schemas: Vec<_> = fs::read_dir(&schema_dir)?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "cue"))
            .collect();

        // Sort by filename for consistent ordering
        schemas.sort_by_key(std::fs::DirEntry::path);

        for entry in schemas {
            let name = entry.file_name();
            let content = fs::read_to_string(entry.path())?;
            write!(
                output,
                "\n### {}\n```cue\n{}\n```\n",
                name.to_string_lossy(),
                content.trim()
            )?;
        }

        fs::write(&output_path, output)?;
    } else {
        // Create minimal placeholder for crates.io builds
        fs::write(
            &output_path,
            "# cuenv\n\nFor full documentation, visit https://github.com/cuenv/cuenv\n",
        )?;
    }

    Ok(())
}
