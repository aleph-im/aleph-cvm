use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use qapi::Stream;
use tracing::debug;

#[allow(dead_code)]
type QmpStream = Stream<BufReader<UnixStream>, UnixStream>;

/// A synchronous QMP (QEMU Machine Protocol) client.
///
/// Connects to a QEMU instance via a Unix domain socket and provides
/// methods for querying VM status and controlling execution.
#[allow(dead_code)]
pub struct QmpClient {
    qmp: qapi::Qmp<QmpStream>,
}

#[allow(dead_code)]
impl QmpClient {
    /// Connect to a QMP socket, retrying up to `retries` times.
    ///
    /// QEMU may not have the socket ready immediately after spawning,
    /// so we retry with a short delay between attempts.
    pub fn connect(socket_path: &Path, retries: u32) -> Result<Self> {
        let mut last_err = None;

        for attempt in 0..=retries {
            match UnixStream::connect(socket_path) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .context("set_read_timeout")?;
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .context("set_write_timeout")?;

                    let reader = BufReader::new(stream.try_clone().context("clone stream")?);
                    let qmp_stream = Stream::new(reader, stream);
                    let mut qmp = qapi::Qmp::new(qmp_stream);

                    qmp.handshake()
                        .map_err(|e| anyhow::anyhow!("QMP handshake failed: {e}"))?;

                    debug!("QMP connected to {}", socket_path.display());
                    return Ok(Self { qmp });
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < retries {
                        debug!(
                            attempt = attempt + 1,
                            max = retries,
                            "QMP connect failed, retrying..."
                        );
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        Err(anyhow::anyhow!(
            "failed to connect to QMP socket {} after {} attempts: {}",
            socket_path.display(),
            retries + 1,
            last_err.unwrap()
        ))
    }

    /// Query the current VM status (e.g. "running", "paused").
    pub fn query_status(&mut self) -> Result<String> {
        let info = self
            .qmp
            .execute(&qapi_qmp::query_status {})
            .map_err(|e| anyhow::anyhow!("query-status failed: {e}"))?;

        Ok(format!("{:?}", info.status))
    }

    /// Request QEMU to quit (terminates the VM).
    pub fn quit(&mut self) -> Result<()> {
        self.qmp
            .execute(&qapi_qmp::quit {})
            .map_err(|e| anyhow::anyhow!("quit failed: {e}"))?;
        Ok(())
    }

    /// Pause the VM.
    pub fn stop(&mut self) -> Result<()> {
        self.qmp
            .execute(&qapi_qmp::stop {})
            .map_err(|e| anyhow::anyhow!("stop failed: {e}"))?;
        Ok(())
    }

    /// Resume the VM.
    pub fn cont(&mut self) -> Result<()> {
        self.qmp
            .execute(&qapi_qmp::cont {})
            .map_err(|e| anyhow::anyhow!("cont failed: {e}"))?;
        Ok(())
    }
}
