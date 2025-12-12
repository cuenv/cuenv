//! Build script to generate llms-full.txt by concatenating llms.txt with CUE schemas.
//!
//! This script looks for llms.txt and schema/ in the workspace root.
//! When building from crates.io (where these files aren't included), it creates
//! a minimal placeholder file instead.

use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

fn main() {
    // Use CARGO_MANIFEST_DIR to get workspace root reliably
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().and_then(|p| p.parent());

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let output_path = format!("{out_dir}/llms-full.txt");

    // Try to find llms.txt and schema/ in workspace root
    let (llms_path, schema_dir) = match workspace_root {
        Some(root) => {
            let llms = root.join("llms.txt");
            let schema = root.join("schema");
            if llms.exists() && schema.exists() {
                println!("cargo::rerun-if-changed={}", llms.display());
                println!("cargo::rerun-if-changed={}", schema.display());
                (Some(llms), Some(schema))
            } else {
                (None, None)
            }
        }
        None => (None, None),
    };

    // If files exist, generate full llms-full.txt
    if let (Some(llms_path), Some(schema_dir)) = (llms_path, schema_dir) {
        let mut output = fs::read_to_string(&llms_path).expect("Failed to read llms.txt");

        output.push_str("\n## CUE Schema Reference\n");

        // Read all .cue files from schema/
        let mut schemas: Vec<_> = fs::read_dir(&schema_dir)
            .expect("schema directory not found")
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "cue"))
            .collect();

        // Sort by filename for consistent ordering
        schemas.sort_by_key(std::fs::DirEntry::path);

        for entry in schemas {
            let name = entry.file_name();
            let content = fs::read_to_string(entry.path())
                .unwrap_or_else(|_| panic!("Failed to read {}", entry.path().display()));
            let _ = write!(
                output,
                "\n### {}\n```cue\n{}\n```\n",
                name.to_string_lossy(),
                content.trim()
            );
        }

        fs::write(&output_path, output).expect("Failed to write llms-full.txt");
    } else {
        // Create minimal placeholder for crates.io builds
        fs::write(
            &output_path,
            "# cuenv\n\nFor full documentation, visit https://github.com/cuenv/cuenv\n",
        )
        .expect("Failed to write llms-full.txt placeholder");
    }
}
