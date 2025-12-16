//! Build script for cuenv-iac crate
//!
//! Generates Rust bindings from Terraform provider protocol v6 protobuf definitions.
//!
//! If protoc is not available, the build will use the checked-in placeholder file.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Re-run if proto file changes
    println!("cargo:rerun-if-changed=proto/tfplugin6.proto");

    // Check if protoc is available
    let protoc_available = std::process::Command::new("protoc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if protoc_available {
        // Compile tfplugin6 proto file with client-side code generation
        tonic_build::configure()
            .build_client(true)
            .build_server(false)
            .out_dir("src/proto")
            .compile_protos(&["proto/tfplugin6.proto"], &["proto/"])?;
    } else {
        // Use the checked-in placeholder file
        println!("cargo:warning=protoc not found, using placeholder proto definitions");
    }

    Ok(())
}
