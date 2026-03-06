//! Adapter layer — translates Aleph messages into compute-node gRPC requests.
//!
//! This is the core translation layer. It resolves Aleph item hashes into
//! local file paths (via the VolumeCache), then builds `CreateVmRequest`
//! proto messages for the compute node.

use anyhow::{Context, Result};
use tracing::info;

use aleph_compute_proto::compute::{
    AddPortForwardRequest, CreateVmRequest, DiskConfig as ProtoDiskConfig,
    TeeConfig as ProtoTeeConfig,
};

use crate::aleph::messages::{ExecutableMessage, MachineVolume};
use crate::aleph::volumes::{VolumeCache, VolumeCategory};

/// Configuration for the adapter.
pub struct AdapterConfig {
    /// Default kernel path (for instances without custom kernel).
    pub kernel_path: String,
    /// Default initrd path.
    pub initrd_path: String,
}

/// Translate an Aleph `ExecutableMessage` into a `CreateVmRequest`.
///
/// Downloads any referenced volumes first, then maps them to disk configs.
pub async fn translate_message(
    msg: &ExecutableMessage,
    cache: &VolumeCache,
    config: &AdapterConfig,
) -> Result<CreateVmRequest> {
    let mut disks = Vec::new();

    // ── Root filesystem (instances) ─────────────────────────────────────────
    if let Some(ref rootfs) = msg.rootfs {
        let path = cache
            .ensure_cached(&rootfs.parent.item_ref, VolumeCategory::Runtime)
            .await
            .context("downloading rootfs")?;

        disks.push(ProtoDiskConfig {
            path: path.display().to_string(),
            readonly: false,
            format: "raw".to_string(),
        });
    }

    // ── Runtime (programs) ──────────────────────────────────────────────────
    if let Some(ref runtime) = msg.runtime {
        let path = cache
            .ensure_cached(&runtime.item_ref, VolumeCategory::Runtime)
            .await
            .context("downloading runtime")?;

        disks.push(ProtoDiskConfig {
            path: path.display().to_string(),
            readonly: true,
            format: "squashfs".to_string(),
        });
    }

    // ── Code (programs) ─────────────────────────────────────────────────────
    if let Some(ref code) = msg.code {
        let path = cache
            .ensure_cached(&code.item_ref, VolumeCategory::Code)
            .await
            .context("downloading code")?;

        disks.push(ProtoDiskConfig {
            path: path.display().to_string(),
            readonly: true,
            format: format!("{:?}", code.encoding).to_lowercase(),
        });
    }

    // ── Additional volumes ──────────────────────────────────────────────────
    for volume in &msg.volumes {
        match volume {
            MachineVolume::Immutable(v) => {
                let path = cache
                    .ensure_cached(&v.item_ref, VolumeCategory::Data)
                    .await
                    .with_context(|| format!("downloading volume {}", v.item_ref))?;

                disks.push(ProtoDiskConfig {
                    path: path.display().to_string(),
                    readonly: true,
                    format: "raw".to_string(),
                });
            }
            MachineVolume::Persistent(v) => {
                if let Some(ref parent) = v.parent {
                    let path = cache
                        .ensure_cached(&parent.item_ref, VolumeCategory::Data)
                        .await
                        .with_context(|| {
                            format!("downloading parent volume {}", parent.item_ref)
                        })?;

                    disks.push(ProtoDiskConfig {
                        path: path.display().to_string(),
                        readonly: false,
                        format: "qcow2".to_string(),
                    });
                }
                // Persistent volumes without a parent are created as empty
                // ext4 filesystems by the compute node.
            }
            MachineVolume::Ephemeral(_) => {
                // Ephemeral volumes are created at VM start and discarded.
                // The compute node handles these internally.
            }
        }
    }

    // ── TEE configuration ───────────────────────────────────────────────────
    let tee = if msg.is_confidential() {
        let policy = msg.sev_policy().unwrap_or(0);
        Some(ProtoTeeConfig {
            backend: "sev-snp".to_string(),
            policy: if policy > 0 {
                format!("0x{policy:x}")
            } else {
                String::new()
            },
        })
    } else {
        None
    };

    let request = CreateVmRequest {
        vm_id: msg.item_hash.clone(),
        kernel: config.kernel_path.clone(),
        initrd: config.initrd_path.clone(),
        disks,
        vcpus: msg.resources.vcpus,
        memory_mb: msg.resources.memory,
        tee,
        ipv6_address: String::new(),
        ipv6_prefix_len: 0,
        encrypted: msg.is_encrypted(),
    };

    info!(
        vm_id = %request.vm_id,
        vcpus = request.vcpus,
        memory_mb = request.memory_mb,
        disks = request.disks.len(),
        confidential = msg.is_confidential(),
        "translated message to CreateVmRequest"
    );

    Ok(request)
}

/// Build port forwarding requests from an Aleph message's published ports.
pub fn translate_port_forwards(msg: &ExecutableMessage) -> Vec<AddPortForwardRequest> {
    msg.resources
        .published_ports
        .iter()
        .map(|p| AddPortForwardRequest {
            vm_id: msg.item_hash.clone(),
            host_port: 0, // auto-allocate
            vm_port: p.port as u32,
            protocol: p.protocol.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aleph::messages::*;

    #[test]
    fn test_translate_port_forwards() {
        let msg = ExecutableMessage {
            item_hash: "test_hash".into(),
            machine_type: MachineType::VmInstance,
            resources: MachineResources {
                vcpus: 1,
                memory: 128,
                seconds: 1,
                published_ports: vec![
                    PublishedPort {
                        protocol: "tcp".into(),
                        port: 8080,
                    },
                    PublishedPort {
                        protocol: "udp".into(),
                        port: 9090,
                    },
                ],
            },
            volumes: vec![],
            variables: Default::default(),
            allow_amend: false,
            replaces: None,
            payment: None,
            environment: None,
            rootfs: None,
            code: None,
            runtime: None,
        };

        let forwards = translate_port_forwards(&msg);
        assert_eq!(forwards.len(), 2);
        assert_eq!(forwards[0].vm_port, 8080);
        assert_eq!(forwards[0].protocol, "tcp");
        assert_eq!(forwards[0].host_port, 0); // auto-allocate
        assert_eq!(forwards[1].vm_port, 9090);
        assert_eq!(forwards[1].protocol, "udp");
    }
}
