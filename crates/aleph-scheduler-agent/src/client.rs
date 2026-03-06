//! gRPC client for connecting to the compute-node over a Unix domain socket.

use std::path::Path;

use anyhow::Result;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use aleph_compute_proto::compute::compute_node_client::ComputeNodeClient;

/// Connect to the compute-node gRPC server over a Unix domain socket.
pub async fn connect_compute_node(
    socket_path: &Path,
) -> Result<ComputeNodeClient<Channel>> {
    let socket_path = socket_path.to_path_buf();

    // tonic requires a valid URI even for UDS; the host is ignored.
    // Wrap tokio::net::UnixStream with hyper_util::rt::TokioIo so it
    // implements the hyper rt::Read + rt::Write traits.
    let channel = Endpoint::try_from("http://[::]:0")?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;

    Ok(ComputeNodeClient::new(channel))
}
