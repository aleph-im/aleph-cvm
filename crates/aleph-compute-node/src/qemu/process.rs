use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::args::QemuPaths;

/// A running QEMU process with associated runtime paths.
pub struct QemuProcess {
    child: Child,
    #[allow(dead_code)]
    pub paths: QemuPaths,
    pub vm_id: String,
}

impl QemuProcess {
    /// Spawn a QEMU process with the given command-line arguments.
    ///
    /// Creates the VM runtime directory before launching the process.
    pub fn spawn(args: &[String], paths: QemuPaths, vm_id: String) -> Result<Self> {
        // Create the runtime directory for this VM
        let vm_dir = paths
            .qmp_socket
            .parent()
            .context("qmp_socket path has no parent")?;
        std::fs::create_dir_all(vm_dir)
            .with_context(|| format!("failed to create VM runtime dir: {}", vm_dir.display()))?;

        let (program, cmd_args) = args
            .split_first()
            .context("empty argument list")?;

        info!(vm_id = %vm_id, program = %program, args = ?cmd_args, "spawning QEMU");

        let child = Command::new(program)
            .args(cmd_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn QEMU: {program}"))?;

        Ok(Self {
            child,
            paths,
            vm_id,
        })
    }

    /// Wait for the QEMU process to exit, killing it after `timeout`.
    pub fn wait_or_kill(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;

        loop {
            match self.child.try_wait()? {
                Some(status) => {
                    if let Some(stderr) = self.child.stderr.take() {
                        use std::io::Read;
                        let mut buf = String::new();
                        let mut reader = stderr;
                        let _ = reader.read_to_string(&mut buf);
                        if !buf.is_empty() {
                            warn!(vm_id = %self.vm_id, stderr = %buf.trim(), "QEMU stderr");
                        }
                    }
                    info!(vm_id = %self.vm_id, ?status, "QEMU process exited");
                    return Ok(());
                }
                None => {
                    if Instant::now() >= deadline {
                        warn!(vm_id = %self.vm_id, "QEMU did not exit in time, sending SIGKILL");
                        self.child.kill().context("failed to kill QEMU")?;
                        self.child.wait().context("failed to wait after kill")?;
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    /// Return the PID of the QEMU process, if still running.
    #[allow(dead_code)]
    pub fn pid(&self) -> Option<u32> {
        Some(self.child.id())
    }
}

impl Drop for QemuProcess {
    fn drop(&mut self) {
        // Best-effort cleanup: try to kill the process if still running
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
