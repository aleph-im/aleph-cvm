use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use ipnet::Ipv6Net;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};
use tracing::info;

use aleph_compute_proto::compute::compute_node_server::{
    ComputeNode, ComputeNodeServer as TonicServer,
};
use aleph_compute_proto::compute::{
    AddPortForwardRequest, CreateVmRequest, DeleteVmRequest, DeleteVmResponse, GetVmRequest,
    HealthRequest, HealthResponse, ListPortForwardsRequest, ListPortForwardsResponse,
    ListVmsRequest, ListVmsResponse, PortForwardInfo, RemovePortForwardRequest,
    RemovePortForwardResponse, VmInfo,
};
use aleph_network::types::Protocol;
use aleph_tee::types::{DiskConfig, DiskRole, TeeConfig, TeeType, VmConfig};

use crate::vm::VmManager;
use crate::vm::manager::classify_disks;

/// Maximum VM ID length. Linux TAP interface names are limited to 15 chars
/// (IFNAMSIZ - 1), and we prefix with "tap-" (4 chars), leaving 11 for the ID.
const MAX_VM_ID_LEN: usize = 11;

/// Validate a VM ID: must be 1-11 chars, lowercase alphanumeric and hyphens,
/// must start with a letter or digit, must not start/end with hyphen.
fn validate_vm_id(vm_id: &str) -> Result<(), Status> {
    if vm_id.is_empty() {
        return Err(Status::invalid_argument("vm_id must not be empty"));
    }
    if vm_id.len() > MAX_VM_ID_LEN {
        return Err(Status::invalid_argument(format!(
            "vm_id too long: max {MAX_VM_ID_LEN} chars (TAP name limit), got {}",
            vm_id.len()
        )));
    }
    if !vm_id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(Status::invalid_argument(
            "vm_id must contain only lowercase alphanumeric characters and hyphens",
        ));
    }
    if vm_id.starts_with('-') || vm_id.ends_with('-') {
        return Err(Status::invalid_argument(
            "vm_id must not start or end with a hyphen",
        ));
    }
    Ok(())
}

/// Validate that a file path exists and is an absolute path.
fn validate_file_path(path: &str, field_name: &str) -> Result<(), Status> {
    if path.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must not be empty"
        )));
    }
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must be an absolute path"
        )));
    }
    if !p.exists() {
        return Err(Status::invalid_argument(format!(
            "{field_name} does not exist: {path}"
        )));
    }
    Ok(())
}

/// Validate VM resource configuration.
fn validate_vm_resources(vcpus: u32, memory_mb: u32) -> Result<(), Status> {
    if vcpus == 0 {
        return Err(Status::invalid_argument("vcpus must be > 0"));
    }
    if memory_mb == 0 {
        return Err(Status::invalid_argument("memory_mb must be > 0"));
    }
    Ok(())
}

/// gRPC server wrapping the VmManager.
pub struct ComputeNodeServer {
    manager: Arc<VmManager>,
}

impl ComputeNodeServer {
    pub fn new(manager: Arc<VmManager>) -> Self {
        Self { manager }
    }

    pub async fn serve(self, socket_path: &Path) -> anyhow::Result<()> {
        // Remove stale socket if it exists
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let uds = UnixListener::bind(socket_path)?;
        // Restrict gRPC socket to owner-only (0600).
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
        let uds_stream = UnixListenerStream::new(uds);

        info!(socket = %socket_path.display(), "gRPC server listening");

        let reflection = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(aleph_compute_proto::compute::FILE_DESCRIPTOR_SET)
            .build_v1()?;

        let shutdown = async {
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .expect("failed to install SIGINT handler");
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to install SIGTERM handler");
            tokio::select! {
                _ = sigint.recv() => info!("received SIGINT, shutting down"),
                _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
            }
        };

        tonic::transport::Server::builder()
            .add_service(reflection)
            .add_service(TonicServer::new(ComputeNodeService {
                manager: self.manager,
            }))
            .serve_with_incoming_shutdown(uds_stream, shutdown)
            .await?;

        // Clean up socket on graceful shutdown
        let _ = std::fs::remove_file(socket_path);

        Ok(())
    }
}

struct ComputeNodeService {
    manager: Arc<VmManager>,
}

fn parse_tee_config(
    proto: Option<aleph_compute_proto::compute::TeeConfig>,
) -> Result<TeeConfig, Status> {
    let proto = proto.unwrap_or_default();
    let backend = match proto.backend.as_str() {
        "sev-snp" | "" => TeeType::SevSnp,
        "tdx" => TeeType::Tdx,
        "nvidia-cc" => TeeType::NvidiaCc,
        "none" => TeeType::None,
        other => {
            return Err(Status::invalid_argument(format!(
                "unknown TEE backend: {other}"
            )));
        }
    };
    let policy = if proto.policy.is_empty() {
        None
    } else {
        Some(proto.policy)
    };
    Ok(TeeConfig { backend, policy })
}

fn vm_info_to_proto(info: &crate::vm::VmInfo) -> VmInfo {
    VmInfo {
        vm_id: info.vm_id.clone(),
        status: info.status.clone(),
        ipv4: info.ip.clone(),
        ipv6: info.ipv6.clone(),
        tee_backend: info.tee.clone(),
        uptime_secs: info.uptime_secs,
        numa_node: info.numa_node.unwrap_or(0),
    }
}

#[tonic::async_trait]
impl ComputeNode for ComputeNodeService {
    async fn create_vm(
        &self,
        request: Request<CreateVmRequest>,
    ) -> Result<Response<VmInfo>, Status> {
        let req = request.into_inner();

        // Validate inputs
        validate_vm_id(&req.vm_id)?;

        // Parse TEE config first (need it for validation decisions)
        let tee = parse_tee_config(req.tee)?;

        // Kernel/initrd: required for confidential VMs, optional for TeeType::None
        let kernel = if req.kernel.is_empty() {
            if tee.backend != TeeType::None {
                return Err(Status::invalid_argument(
                    "kernel is required for confidential VMs",
                ));
            }
            None
        } else {
            validate_file_path(&req.kernel, "kernel")?;
            Some(req.kernel.into())
        };

        let initrd = if req.initrd.is_empty() {
            if tee.backend != TeeType::None {
                return Err(Status::invalid_argument(
                    "initrd is required for confidential VMs",
                ));
            }
            None
        } else {
            validate_file_path(&req.initrd, "initrd")?;
            Some(req.initrd.into())
        };

        validate_vm_resources(req.vcpus, req.memory_mb)?;

        if tee.backend == TeeType::None && req.encrypted {
            return Err(Status::invalid_argument(
                "encrypted mode is not supported for non-confidential VMs",
            ));
        }

        for d in &req.disks {
            validate_file_path(&d.path, "disk path")?;
            // Validate disk format against allowlist to prevent QEMU parameter injection.
            let fmt = if d.format.is_empty() {
                "raw"
            } else {
                &d.format
            };
            if fmt != "raw" && fmt != "qcow2" {
                return Err(Status::invalid_argument(format!(
                    "unsupported disk format: {fmt} (allowed: raw, qcow2)"
                )));
            }
            // Reject paths containing commas (QEMU option separator).
            if d.path.contains(',') {
                return Err(Status::invalid_argument(
                    "disk path must not contain commas",
                ));
            }
            // Validate disk role against allowlist.
            match d.role.as_str() {
                "" | "rootfs" | "workload" | "verified_volume" => {}
                other => {
                    return Err(Status::invalid_argument(format!(
                        "unknown disk role {other:?}; expected rootfs, workload, or verified_volume"
                    )));
                }
            }
        }

        // Non-confidential disk-boot VMs must have at least one disk
        if kernel.is_none() && req.disks.is_empty() {
            return Err(Status::invalid_argument(
                "at least one disk is required for disk-boot VMs (no kernel specified)",
            ));
        }

        let disks = req
            .disks
            .into_iter()
            .map(|d| {
                let role = match d.role.as_str() {
                    "" => DiskRole::Unspecified,
                    "rootfs" => DiskRole::Rootfs,
                    "workload" => DiskRole::Workload,
                    "verified_volume" => DiskRole::VerifiedVolume,
                    other => unreachable!("disk role {other:?} already validated"),
                };
                DiskConfig {
                    path: d.path.into(),
                    readonly: d.readonly,
                    format: if d.format.is_empty() {
                        "raw".to_string()
                    } else {
                        d.format
                    },
                    role,
                }
            })
            .collect::<Vec<_>>();

        // Validate disk ordering (role mode vs. legacy positional mode,
        // rootfs-first, at most one workload, workload immediately after
        // rootfs) before any host-side side effect (IP allocation, TAP
        // creation, nftables setup) happens in the manager. This runs for
        // every CreateVm, unlike the manager's own classify_disks call
        // (vm/manager.rs), which is skipped for LUKS-encrypted and
        // non-confidential VMs.
        classify_disks(&disks).map_err(|e| Status::invalid_argument(e.to_string()))?;

        // Parse IPv6 request
        let requested_ipv6 = if req.ipv6_address.is_empty() {
            None
        } else {
            let addr: std::net::Ipv6Addr = req
                .ipv6_address
                .parse()
                .map_err(|e| Status::invalid_argument(format!("invalid ipv6_address: {e}")))?;
            let prefix = if req.ipv6_prefix_len == 0 {
                128
            } else {
                req.ipv6_prefix_len as u8
            };
            let net = Ipv6Net::new(addr, prefix)
                .map_err(|e| Status::invalid_argument(format!("invalid IPv6 prefix: {e}")))?;
            Some(net)
        };

        let numa_hint = if req.numa_node == 0 {
            None
        } else {
            Some(req.numa_node - 1) // 1-indexed API -> 0-indexed internal
        };

        let config = VmConfig {
            vm_id: req.vm_id,
            kernel,
            initrd,
            disks,
            vcpus: req.vcpus,
            memory_mb: req.memory_mb,
            tee,
            encrypted: req.encrypted,
            numa_node: None,
            hugepage_size: None,
        };

        let info = self
            .manager
            .create_vm(config, requested_ipv6, numa_hint)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(vm_info_to_proto(&info)))
    }

    async fn get_vm(&self, request: Request<GetVmRequest>) -> Result<Response<VmInfo>, Status> {
        let req = request.into_inner();

        let info = self
            .manager
            .get_vm(&req.vm_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(vm_info_to_proto(&info)))
    }

    async fn delete_vm(
        &self,
        request: Request<DeleteVmRequest>,
    ) -> Result<Response<DeleteVmResponse>, Status> {
        let req = request.into_inner();

        self.manager
            .delete_vm(&req.vm_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(DeleteVmResponse {}))
    }

    async fn list_vms(
        &self,
        _request: Request<ListVmsRequest>,
    ) -> Result<Response<ListVmsResponse>, Status> {
        let vms = self.manager.list_vms().await;
        Ok(Response::new(ListVmsResponse {
            vms: vms.iter().map(vm_info_to_proto).collect(),
        }))
    }

    async fn add_port_forward(
        &self,
        request: Request<AddPortForwardRequest>,
    ) -> Result<Response<PortForwardInfo>, Status> {
        let req = request.into_inner();

        let protocol: Protocol = req
            .protocol
            .parse()
            .map_err(|e: anyhow::Error| Status::invalid_argument(e.to_string()))?;

        let forward = self
            .manager
            .add_port_forward(
                &req.vm_id,
                req.host_port as u16,
                req.vm_port as u16,
                protocol,
            )
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(PortForwardInfo {
            vm_id: forward.vm_id,
            host_port: forward.host_port as u32,
            vm_port: forward.vm_port as u32,
            protocol: forward.protocol.to_string(),
        }))
    }

    async fn remove_port_forward(
        &self,
        request: Request<RemovePortForwardRequest>,
    ) -> Result<Response<RemovePortForwardResponse>, Status> {
        let req = request.into_inner();

        let protocol: Protocol = req
            .protocol
            .parse()
            .map_err(|e: anyhow::Error| Status::invalid_argument(e.to_string()))?;

        self.manager
            .remove_port_forward(req.host_port as u16, protocol)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(RemovePortForwardResponse {}))
    }

    async fn list_port_forwards(
        &self,
        request: Request<ListPortForwardsRequest>,
    ) -> Result<Response<ListPortForwardsResponse>, Status> {
        let req = request.into_inner();

        let vm_id = if req.vm_id.is_empty() {
            None
        } else {
            Some(req.vm_id.as_str())
        };

        let forwards = self.manager.list_port_forwards(vm_id).await;

        Ok(Response::new(ListPortForwardsResponse {
            forwards: forwards
                .into_iter()
                .map(|f| PortForwardInfo {
                    vm_id: f.vm_id,
                    host_port: f.host_port as u32,
                    vm_port: f.vm_port as u32,
                    protocol: f.protocol.to_string(),
                })
                .collect(),
        }))
    }

    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let vm_count = self.manager.list_vms().await.len() as u32;
        Ok(Response::new(HealthResponse {
            status: "ok".to_string(),
            vm_count,
        }))
    }
}
