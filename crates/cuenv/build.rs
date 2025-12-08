//! Build script to generate llms-full.txt by concatenating llms.txt with CUE schemas.

use std::fmt::Write;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo::rerun-if-changed=../../llms.txt");
    println!("cargo::rerun-if-changed=../../schema");

    let llms_path = Path::new("../../llms.txt");
    let schema_dir = Path::new("../../schema");

    let mut output = fs::read_to_string(llms_path).expect("llms.txt not found at project root");

    output.push_str("\n## CUE Schema Reference\n");

    // Read all .cue files from schema/
    let mut schemas: Vec<_> = fs::read_dir(schema_dir)
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

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    fs::write(format!("{out_dir}/llms-full.txt"), output).expect("Failed to write llms-full.txt");
}
