fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile REAPI protos with tonic-build
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        // Make Digest hashable for use in HashMaps
        .type_attribute(
            "build.bazel.remote.execution.v2.Digest",
            "#[derive(Eq, Hash)]",
        )
        // Compile the main REAPI proto and its dependencies
        .compile_protos(
            &[
                "proto/build/bazel/remote/execution/v2/remote_execution.proto",
                "proto/build/bazel/semver/semver.proto",
                "proto/google/bytestream/bytestream.proto",
                "proto/google/longrunning/operations.proto",
            ],
            &["proto"],
        )?;

    Ok(())
}
