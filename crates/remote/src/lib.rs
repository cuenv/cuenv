pub mod error;
pub mod merkle;

pub mod reapi {
    pub mod build {
        pub mod bazel {
            pub mod remote {
                pub mod execution {
                    pub mod v2 {
                        tonic::include_proto!("build.bazel.remote.execution.v2");
                    }
                }
            }
            pub mod semver {
                tonic::include_proto!("build.bazel.semver");
            }
        }
    }
    pub mod google {
        pub mod longrunning {
            tonic::include_proto!("google.longrunning");
        }
        pub mod rpc {
            tonic::include_proto!("google.rpc");
        }
        pub mod api {
            tonic::include_proto!("google.api");
        }
    }
}

pub use error::RemoteError;