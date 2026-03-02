pub mod compute {
    tonic::include_proto!("aleph.compute.v1");

    /// Encoded file descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("compute_descriptor");
}

pub use compute::*;
