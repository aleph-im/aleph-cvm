use std::collections::{BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ipnet::Ipv6Net;
use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

use aleph_network::ipv4::port_forward::PortForwardState;
use aleph_network::ipv6::Ipv6RangeAllocator;
use aleph_network::ndp_proxy::NdpProxy;
use aleph_network::nftables::NftablesManager;
use aleph_network::types::{PortForward, Protocol};
use aleph_tee::traits::TeeBackend;
use aleph_tee::types::VmConfig;

use crate::network;
use crate::numa::{NumaAllocator, NumaTopology};
use crate::persistence::{self, PersistedVm};
use crate::qemu::args::{QemuPaths, build_qemu_command};
use crate::qemu::process::QemuProcess;
use crate::verity;
use crate::vm::lifecycle::VmState;

/// Internal handle for a managed VM.
struct VmHandle {
    config: VmConfig,
    state: VmState,
    ip: Ipv4Addr,
    ipv6: Option<Ipv6Net>,
    process: Option<QemuProcess>,
    tap_name: String,
    /// Wall-clock creation time (seconds since UNIX epoch).
    /// Used for uptime calculation that survives orchestrator restarts.
    created_at_epoch: u64,
    numa_node: Option<u32>,
}

/// JSON-serializable VM information returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct VmInfo {
    pub vm_id: String,
    pub status: String,
    pub ip: String,
    pub ipv6: String,
    pub tee: String,
    pub uptime_secs: u64,
    pub numa_node: Option<u32>,
}

impl VmInfo {
    fn from_handle(handle: &VmHandle) -> Self {
        Self {
            vm_id: handle.config.vm_id.clone(),
            status: handle.state.to_string(),
            ip: handle.ip.to_string(),
            ipv6: handle.ipv6.map(|n| n.to_string()).unwrap_or_default(),
            tee: format!("{:?}", handle.config.tee.backend),
            uptime_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_sub(handle.created_at_epoch),
            numa_node: handle.numa_node,
        }
    }
}

/// Manages the lifecycle of confidential VMs.
pub struct VmManager {
    vms: RwLock<HashMap<String, VmHandle>>,
    run_dir: PathBuf,
    state_dir: PathBuf,
    bridge: String,
    gateway_ip: Ipv4Addr,
    /// Tracks used IP offsets from the gateway. Offsets are reclaimed on VM deletion.
    used_ip_offsets: RwLock<BTreeSet<u8>>,
    tee_backend: Arc<dyn TeeBackend>,
    dhcp_hostsdir: Option<PathBuf>,
    nftables: NftablesManager,
    port_forwards: Mutex<PortForwardState>,
    ipv6_allocator: Option<Mutex<Ipv6RangeAllocator>>,
    ndp_proxy: Option<Arc<NdpProxy>>,
    numa: Mutex<NumaAllocator>,
}

impl VmManager {
    /// Create a new VM manager.
    ///
    /// If `dhcp_hostsdir` is provided, the manager writes per-VM dnsmasq
    /// DHCP host reservation files so that VMs get their assigned IP via DHCP.
    ///
    /// If `ipv6_pool` is provided, IPv6 addresses are allocated from that pool
    /// and NDP proxy + ip6 nftables chains are enabled.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_dir: PathBuf,
        state_dir: PathBuf,
        bridge: String,
        gateway_ip: Ipv4Addr,
        tee_backend: Arc<dyn TeeBackend>,
        dhcp_hostsdir: Option<PathBuf>,
        external_interface: String,
        ipv6_pool: Option<Ipv6Net>,
        use_ndp_proxy: bool,
        numa_topology: NumaTopology,
    ) -> Self {
        let ipv6_enabled = ipv6_pool.is_some();
        let nftables = NftablesManager::new("aleph", &external_interface, ipv6_enabled);

        let ipv6_allocator = ipv6_pool.map(|pool| Mutex::new(Ipv6RangeAllocator::new(pool, 128)));

        let ndp_proxy = if ipv6_enabled && use_ndp_proxy {
            Some(Arc::new(NdpProxy::new(&external_interface)))
        } else {
            None
        };

        Self {
            vms: RwLock::new(HashMap::new()),
            run_dir,
            state_dir,
            bridge,
            gateway_ip,
            used_ip_offsets: RwLock::new(BTreeSet::new()),
            tee_backend,
            dhcp_hostsdir,
            nftables,
            port_forwards: Mutex::new(PortForwardState::new()),
            ipv6_allocator,
            ndp_proxy,
            numa: Mutex::new(NumaAllocator::new(numa_topology)),
        }
    }

    /// Initialize nftables supervisor chains. Call once at startup.
    pub fn setup_nftables(&self) -> Result<()> {
        self.nftables.initialize()
    }

    /// Create and start a new VM.
    ///
    /// If `requested_ipv6` is `Some`, the address is validated and allocated.
    /// If `None` and an IPv6 pool is configured, a random /128 is allocated.
    pub async fn create_vm(
        &self,
        mut config: VmConfig,
        requested_ipv6: Option<Ipv6Net>,
        numa_hint: Option<u32>,
    ) -> Result<VmInfo> {
        let vm_id = config.vm_id.clone();

        // Check for duplicate
        {
            let vms = self.vms.read().await;
            if vms.contains_key(&vm_id) {
                anyhow::bail!("VM {vm_id} already exists");
            }
        }

        // Allocate IP and derive MAC — find lowest free offset (1..=254).
        // Offset 0 is the gateway itself; 255 is broadcast.
        let offset = {
            let mut used = self.used_ip_offsets.write().await;
            let free = (1u8..=254).find(|o| !used.contains(o));
            match free {
                Some(o) => {
                    used.insert(o);
                    o
                }
                None => anyhow::bail!("IPv4 address pool exhausted (254 VMs max)"),
            }
        };
        let vm_ip = network::allocate_vm_ip(self.gateway_ip, offset);
        let mac_addr = network::mac_for_vm_ip(vm_ip);

        // Allocate IPv6 from pool (if enabled)
        let vm_ipv6 = if let Some(ref alloc) = self.ipv6_allocator {
            let mut alloc = alloc.lock().await;
            Some(alloc.allocate(&vm_id, requested_ipv6)?)
        } else {
            None
        };

        // Write DHCP reservation so dnsmasq assigns this IP to the VM's MAC
        if let Some(ref hostsdir) = self.dhcp_hostsdir {
            network::write_dhcp_reservation(hostsdir, &vm_id, &mac_addr, vm_ip)
                .context("failed to write DHCP reservation")?;

            // Write DHCPv6 reservation if IPv6 is allocated
            if let Some(ref ipv6) = vm_ipv6
                && let Err(e) =
                    network::write_dhcpv6_reservation(hostsdir, &vm_id, &mac_addr, ipv6.addr())
            {
                network::remove_dhcp_reservation(hostsdir, &vm_id);
                if let Some(ref alloc) = self.ipv6_allocator {
                    alloc.lock().await.release(&vm_id);
                }
                return Err(anyhow::anyhow!(e).context("failed to write DHCPv6 reservation"));
            }
        }

        // Create TAP interface
        let tap_name = format!("tap-{}", &vm_id);
        if let Err(e) = network::create_tap(&tap_name, &self.bridge).await {
            error!(vm_id = %vm_id, error = %e, "failed to create TAP interface");
            self.cleanup_reservations(&vm_id, vm_ipv6.is_some());
            return Err(e);
        }

        // Add IPv6 address to TAP interface
        if let Some(ref ipv6) = vm_ipv6
            && let Err(e) = network::add_ipv6_to_tap(&tap_name, *ipv6).await
        {
            error!(vm_id = %vm_id, error = %e, "failed to add IPv6 to TAP");
            let _ = network::delete_tap(&tap_name).await;
            self.cleanup_reservations(&vm_id, true);
            return Err(e);
        }

        // Set up nftables per-VM chains (NAT + filter)
        if let Err(e) = self.nftables.setup_vm(&vm_id, &tap_name) {
            error!(vm_id = %vm_id, error = %e, "failed to set up nftables for VM");
            let _ = network::delete_tap(&tap_name).await;
            self.cleanup_reservations(&vm_id, vm_ipv6.is_some());
            return Err(e);
        }

        // Set up NDP proxy for IPv6
        if let (Some(ref ipv6), Some(ndp)) = (vm_ipv6, &self.ndp_proxy) {
            ndp.add_range(&tap_name, *ipv6).await;
        }

        // Compute dm-verity for the rootfs (first disk), or skip if LUKS encrypted
        let encrypted = config.encrypted;
        let kernel_cmdline = if encrypted {
            // LUKS mode: skip dm-verity, user will inject key via attest-agent.
            info!(vm_id = %vm_id, "LUKS encrypted rootfs mode");
            verity::build_kernel_cmdline(None, true)
        } else if let Some(rootfs_disk) = config.disks.first() {
            let vinfo = verity::ensure_verity(&rootfs_disk.path).context(
                "dm-verity setup failed — refusing to boot without integrity verification",
            )?;
            // Insert hash tree as second disk (right after rootfs)
            config.disks.insert(
                1,
                aleph_tee::types::DiskConfig {
                    path: vinfo.hashtree_path,
                    readonly: true,
                    format: "raw".to_string(),
                },
            );
            verity::build_kernel_cmdline(Some(&vinfo.root_hash), false)
        } else {
            verity::build_kernel_cmdline(None, false)
        };

        // Allocate NUMA node — only set numa_node (which triggers QEMU
        // host-nodes binding) when there are multiple nodes. On single-node
        // systems binding is unnecessary and may not be supported by QEMU.
        let placement = {
            let mut numa = self.numa.lock().await;
            let p = numa.allocate(config.vcpus, config.memory_mb, numa_hint)?;
            if numa.num_nodes() > 1 {
                config.numa_node = Some(p.node);
            }
            p
        };

        // Build QEMU command
        let paths = QemuPaths::for_vm(&self.run_dir, &vm_id);
        let mut args = vec!["qemu-system-x86_64".to_string()];
        args.extend(build_qemu_command(
            &config,
            &paths,
            &tap_name,
            self.tee_backend.as_ref(),
            &mac_addr,
            &kernel_cmdline,
        ));

        // Collect parent directories of writable disks for ReadWritePaths.
        let rw_dirs: Vec<&std::path::Path> = config
            .disks
            .iter()
            .filter(|d| !d.readonly)
            .filter_map(|d| d.path.parent())
            .collect();

        // Spawn QEMU
        let process = match QemuProcess::spawn(
            &args,
            paths,
            vm_id.clone(),
            &rw_dirs,
            Some(placement.cpuset.as_str()),
        ) {
            Ok(p) => p,
            Err(e) => {
                error!(vm_id = %vm_id, error = %e, "failed to spawn QEMU");
                // Release NUMA allocation
                {
                    let mut numa = self.numa.lock().await;
                    numa.release(placement.node, config.vcpus, config.memory_mb);
                }
                // Clean up everything on failure
                if let (Some(ref ipv6), Some(ndp)) = (vm_ipv6, &self.ndp_proxy) {
                    ndp.delete_range(&tap_name).await;
                    let _ = ipv6; // suppress unused warning
                }
                let _ = self.nftables.teardown_vm(&vm_id);
                let _ = network::delete_tap(&tap_name).await;
                self.cleanup_reservations(&vm_id, vm_ipv6.is_some());
                return Err(e);
            }
        };

        // Persist VM state to disk
        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let persisted = PersistedVm {
            config: config.clone(),
            ip: vm_ip,
            ipv6: vm_ipv6,
            tap_name: tap_name.clone(),
            mac_addr,
            port_forwards: vec![],
            created_at_epoch: now_epoch,
            numa_node: Some(placement.node),
        };
        if let Err(e) = persistence::save_vm(&self.state_dir, &vm_id, &persisted) {
            warn!(vm_id = %vm_id, error = %e, "failed to persist VM state (VM is running but not recoverable)");
        }

        // Mark as Running immediately (no health check polling yet)
        let handle = VmHandle {
            config,
            state: VmState::Running,
            ip: vm_ip,
            ipv6: vm_ipv6,
            process: Some(process),
            tap_name,
            created_at_epoch: now_epoch,
            numa_node: Some(placement.node),
        };

        let info = VmInfo::from_handle(&handle);

        self.vms.write().await.insert(vm_id.clone(), handle);
        info!(vm_id = %vm_id, ip = %vm_ip, ipv6 = ?vm_ipv6, "VM created");

        Ok(info)
    }

    /// Clean up DHCP/DHCPv6 reservations and IPv6 allocation on failure.
    fn cleanup_reservations(&self, vm_id: &str, has_ipv6: bool) {
        if let Some(ref hostsdir) = self.dhcp_hostsdir {
            network::remove_dhcp_reservation(hostsdir, vm_id);
            if has_ipv6 {
                network::remove_dhcpv6_reservation(hostsdir, vm_id);
            }
        }
        if has_ipv6 && let Some(ref alloc) = self.ipv6_allocator {
            // Best-effort release; can't await in sync context, so use try_lock
            if let Ok(mut alloc) = alloc.try_lock() {
                alloc.release(vm_id);
            }
        }
    }

    /// Get information about a specific VM.
    pub async fn get_vm(&self, id: &str) -> Result<VmInfo> {
        let vms = self.vms.read().await;
        let handle = vms.get(id).with_context(|| format!("VM {id} not found"))?;
        Ok(VmInfo::from_handle(handle))
    }

    /// Delete a VM, stopping it if running.
    pub async fn delete_vm(&self, id: &str) -> Result<()> {
        let mut vms = self.vms.write().await;
        let handle = vms
            .remove(id)
            .with_context(|| format!("VM {id} not found"))?;

        // Stop the QEMU process via systemd if still running
        if let Some(ref process) = handle.process {
            let _ = process.stop();
        }

        // Release NUMA allocation
        if let Some(node) = handle.numa_node {
            let mut numa = self.numa.lock().await;
            numa.release(node, handle.config.vcpus, handle.config.memory_mb);
        }

        // Remove port forwards for this VM
        let removed_forwards = {
            let mut pf = self.port_forwards.lock().await;
            pf.remove_all_for_vm(id)
        };
        for fwd in &removed_forwards {
            let _ = self
                .nftables
                .remove_port_forward(fwd.host_port, fwd.vm_port, fwd.protocol);
        }

        // Remove NDP proxy range
        if let Some(ref ndp) = self.ndp_proxy {
            ndp.delete_range(&handle.tap_name).await;
        }

        // Release IPv6 allocation
        if handle.ipv6.is_some()
            && let Some(ref alloc) = self.ipv6_allocator
        {
            alloc.lock().await.release(id);
        }

        // Tear down nftables chains for this VM
        let _ = self.nftables.teardown_vm(id);

        // Clean up TAP interface
        let _ = network::delete_tap(&handle.tap_name).await;

        // Clean up DHCP/DHCPv6 reservations
        if let Some(ref hostsdir) = self.dhcp_hostsdir {
            network::remove_dhcp_reservation(hostsdir, &handle.config.vm_id);
            if handle.ipv6.is_some() {
                network::remove_dhcpv6_reservation(hostsdir, &handle.config.vm_id);
            }
        }

        // Reclaim IP offset
        {
            let ip_offset = handle.ip.octets()[3].wrapping_sub(self.gateway_ip.octets()[3]);
            self.used_ip_offsets.write().await.remove(&ip_offset);
        }

        // Delete persisted state
        if let Err(e) = persistence::delete_vm(&self.state_dir, id) {
            warn!(vm_id = %id, error = %e, "failed to delete VM state file");
        }

        info!(vm_id = %id, "VM deleted");
        Ok(())
    }

    /// List all VMs.
    #[allow(dead_code)]
    pub async fn list_vms(&self) -> Vec<VmInfo> {
        let vms = self.vms.read().await;
        vms.values().map(VmInfo::from_handle).collect()
    }

    /// Add a port forwarding rule for a VM.
    pub async fn add_port_forward(
        &self,
        vm_id: &str,
        host_port: u16,
        vm_port: u16,
        protocol: Protocol,
    ) -> Result<PortForward> {
        // Validate VM exists and get its IP
        let guest_ip = {
            let vms = self.vms.read().await;
            let handle = vms
                .get(vm_id)
                .with_context(|| format!("VM {vm_id} not found"))?;
            handle.ip
        };

        // Hold the port forward lock for the entire allocate → check → reserve
        // sequence to prevent TOCTOU races between concurrent requests.
        let forward = {
            let mut pf = self.port_forwards.lock().await;

            let actual_host_port = if host_port == 0 {
                pf.auto_allocate(protocol, 10000)
                    .context("no available ports for auto-allocation")?
            } else {
                if !pf.is_available(host_port, protocol) {
                    anyhow::bail!("port {} ({}) is already in use", host_port, protocol);
                }
                host_port
            };

            let forward = PortForward {
                vm_id: vm_id.to_string(),
                host_port: actual_host_port,
                vm_port,
                protocol,
            };

            // Reserve the port in state before releasing the lock.
            pf.add(forward.clone());

            forward
        };

        // Add nftables rules (outside the lock — this is a system call).
        if let Err(e) =
            self.nftables
                .add_port_forward(vm_id, guest_ip, forward.host_port, vm_port, protocol)
        {
            // Roll back the reservation on nftables failure.
            let mut pf = self.port_forwards.lock().await;
            pf.remove(forward.host_port, forward.protocol);
            return Err(e).context("failed to add nftables port forward");
        }

        self.update_persisted_port_forwards(vm_id).await;

        Ok(forward)
    }

    /// Remove a port forwarding rule.
    pub async fn remove_port_forward(&self, host_port: u16, protocol: Protocol) -> Result<()> {
        let forward = {
            let mut pf = self.port_forwards.lock().await;
            pf.remove(host_port, protocol)
                .context("port forward not found")?
        };

        self.nftables
            .remove_port_forward(host_port, forward.vm_port, protocol)?;

        self.update_persisted_port_forwards(&forward.vm_id).await;

        Ok(())
    }

    /// List port forwards, optionally filtered by VM ID.
    pub async fn list_port_forwards(&self, vm_id: Option<&str>) -> Vec<PortForward> {
        let pf = self.port_forwards.lock().await;
        match vm_id {
            Some(id) => pf.list_for_vm(id).into_iter().cloned().collect(),
            None => pf.list_all().into_iter().cloned().collect(),
        }
    }

    /// Re-persist port forwards for a VM after changes.
    async fn update_persisted_port_forwards(&self, vm_id: &str) {
        let path = self.state_dir.join(format!("{vm_id}.json"));
        if let Ok(json) = std::fs::read_to_string(&path)
            && let Ok(mut persisted) = serde_json::from_str::<PersistedVm>(&json)
        {
            let pf = self.port_forwards.lock().await;
            persisted.port_forwards = pf.list_for_vm(vm_id).into_iter().cloned().collect();
            drop(pf);
            let _ = persistence::save_vm(&self.state_dir, vm_id, &persisted);
        }
    }

    /// Recover VMs from persisted state on startup.
    ///
    /// For each saved VM:
    /// - If systemd unit is active -> reconnect (state = Running)
    /// - If systemd unit is not active -> load as Stopped (scheduler decides)
    pub async fn recover_vms(&self) -> Result<()> {
        let persisted_vms = persistence::load_all_vms(&self.state_dir)?;
        if persisted_vms.is_empty() {
            return Ok(());
        }

        info!(
            count = persisted_vms.len(),
            "recovering VMs from persisted state"
        );

        let mut vms = self.vms.write().await;
        let mut used_offsets = self.used_ip_offsets.write().await;

        for pvm in persisted_vms {
            let vm_id = pvm.config.vm_id.clone();
            let paths = QemuPaths::for_vm(&self.run_dir, &vm_id);

            // Register this IP offset as used
            let offset = pvm.ip.octets()[3].wrapping_sub(self.gateway_ip.octets()[3]);
            used_offsets.insert(offset);

            // Try to reconnect to running systemd unit
            let (process, state) = match QemuProcess::reconnect(paths, vm_id.clone()) {
                Ok(p) => (Some(p), VmState::Running),
                Err(_) => (None, VmState::Stopped),
            };

            // Restore NUMA allocation only for running VMs — stopped VMs
            // have no QEMU process and aren't consuming hugepages or CPUs.
            if state == VmState::Running
                && let Some(node) = pvm.numa_node
            {
                let mut numa = self.numa.lock().await;
                let _ = numa
                    .allocate(pvm.config.vcpus, pvm.config.memory_mb, Some(node))
                    .map_err(
                        |e| warn!(vm_id = %vm_id, error = %e, "failed to restore NUMA allocation"),
                    );
            }

            // Re-register IPv6 allocation if applicable
            if let Some(ref ipv6) = pvm.ipv6
                && let Some(ref alloc) = self.ipv6_allocator
            {
                let mut alloc = alloc.lock().await;
                let _ = alloc.allocate(&vm_id, Some(*ipv6));
            }

            // Restore port forwards into in-memory state
            {
                let mut pf = self.port_forwards.lock().await;
                for fwd in &pvm.port_forwards {
                    pf.add(fwd.clone());
                }
            }

            // For running VMs, replay nftables and NDP proxy rules.
            // These are kernel state that may have been lost (e.g., manual
            // nft flush, or host reboot where the VM somehow survived).
            // All nftables helpers are idempotent (*_if_not_present).
            if state == VmState::Running {
                if let Err(e) = self.nftables.setup_vm(&vm_id, &pvm.tap_name) {
                    warn!(vm_id = %vm_id, error = %e, "failed to restore nftables for recovered VM");
                }

                // Replay port forward nftables rules
                for fwd in &pvm.port_forwards {
                    if let Err(e) = self.nftables.add_port_forward(
                        &vm_id,
                        pvm.ip,
                        fwd.host_port,
                        fwd.vm_port,
                        fwd.protocol,
                    ) {
                        warn!(vm_id = %vm_id, port = fwd.host_port, error = %e,
                            "failed to restore port forward for recovered VM");
                    }
                }

                // Replay NDP proxy for IPv6
                if let (Some(ipv6), Some(ndp)) = (&pvm.ipv6, &self.ndp_proxy) {
                    ndp.add_range(&pvm.tap_name, *ipv6).await;
                }
            }

            let handle = VmHandle {
                config: pvm.config,
                state,
                ip: pvm.ip,
                ipv6: pvm.ipv6,
                process,
                tap_name: pvm.tap_name,
                created_at_epoch: pvm.created_at_epoch,
                numa_node: pvm.numa_node,
            };

            info!(
                vm_id = %vm_id,
                state = %handle.state,
                ip = %handle.ip,
                "recovered VM"
            );
            vms.insert(vm_id, handle);
        }

        Ok(())
    }
}
