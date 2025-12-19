fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(
            &[
                "proto/build/bazel/remote/execution/v2/remote_execution.proto",
                "proto/build/bazel/semver/semver.proto",
                "proto/google/longrunning/operations.proto",
                "proto/google/rpc/status.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
