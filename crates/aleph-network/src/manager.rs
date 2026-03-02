use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::info;

use crate::ipv4::port_forward::PortForwardState;
use crate::ipv4::Ipv4Allocator;
use crate::ipv6::Ipv6Allocator;
use crate::ndp_proxy::NdpProxy;
use crate::nftables::NftablesManager;
use crate::types::{NetworkConfig, PortForward, Protocol, TapInterface};

/// Top-level network orchestrator injected into VmManager.
///
/// Combines nftables firewall, IPv4/IPv6 allocation, TAP management,
/// and NDP proxy into a single interface.
pub struct NetworkManager {
    nftables: NftablesManager,
    ipv4_pool: tokio::sync::Mutex<Ipv4Allocator>,
    ipv6_allocator: tokio::sync::Mutex<Box<dyn Ipv6Allocator>>,
    port_forwards: tokio::sync::Mutex<PortForwardState>,
    bridge: String,
    ndp_proxy: Option<Arc<NdpProxy>>,
    config: NetworkConfig,
}

impl NetworkManager {
    pub fn new(config: NetworkConfig, ipv6_allocator: Box<dyn Ipv6Allocator>) -> Self {
        let nftables = NftablesManager::new(
            &config.nftables_prefix,
            &config.external_interface,
            config.ipv6_enabled,
        );

        let ipv4_pool = Ipv4Allocator::new(config.ipv4_pool, config.vm_subnet_prefix);

        let ndp_proxy = if config.use_ndp_proxy {
            Some(Arc::new(NdpProxy::new(&config.external_interface)))
        } else {
            None
        };

        Self {
            nftables,
            ipv4_pool: tokio::sync::Mutex::new(ipv4_pool),
            ipv6_allocator: tokio::sync::Mutex::new(ipv6_allocator),
            port_forwards: tokio::sync::Mutex::new(PortForwardState::new()),
            bridge: config.bridge.clone(),
            ndp_proxy,
            config,
        }
    }

    /// Initialize the network stack (nftables, IP forwarding).
    pub async fn setup(&self) -> Result<()> {
        // Enable IP forwarding
        enable_ipv4_forwarding().await?;
        if self.config.ipv6_enabled {
            enable_ipv6_forwarding().await?;
        }

        // Ensure bridge exists
        crate::bridge::ensure_bridge(
            &self.bridge,
            self.config.ipv4_pool.network(),
            self.config.ipv4_pool.prefix_len(),
        )
        .await?;

        // Initialize nftables chains
        self.nftables.initialize()?;

        info!("network stack initialized");
        Ok(())
    }

    /// Tear down the network stack.
    pub async fn teardown(&self) -> Result<()> {
        self.nftables.teardown()?;
        info!("network stack torn down");
        Ok(())
    }

    /// Create network resources for a VM (allocate IPs, create TAP, set up firewall).
    pub async fn create_vm_network(
        &self,
        vm_id: &str,
        vm_hash: &str,
        vm_type: crate::ipv6::VmType,
    ) -> Result<TapInterface> {
        // Allocate IPv4 subnet
        let ipv4_network = {
            let mut pool = self.ipv4_pool.lock().await;
            pool.allocate()
                .context("IPv4 address pool exhausted")?
        };

        // Allocate IPv6 subnet
        let ipv6_network = {
            let mut alloc = self.ipv6_allocator.lock().await;
            alloc
                .allocate(0, vm_hash, vm_type)
                .context("IPv6 allocation failed")?
        };

        let interface = TapInterface {
            device_name: format!("vmtap{}", vm_id),
            ipv4_network,
            ipv6_network,
        };

        // Create TAP interface
        crate::tap::create_tap(&interface, &self.bridge).await?;

        // Set up nftables rules
        self.nftables.setup_vm(vm_id, &interface.device_name)?;

        // Add NDP proxy range
        if let Some(ref ndp) = self.ndp_proxy {
            ndp.add_range(&interface.device_name, ipv6_network).await;
        }

        info!(
            vm_id = %vm_id,
            ipv4 = %ipv4_network,
            ipv6 = %ipv6_network,
            tap = %interface.device_name,
            "VM network created"
        );

        Ok(interface)
    }

    /// Destroy network resources for a VM.
    pub async fn destroy_vm_network(&self, vm_id: &str, tap: &TapInterface) -> Result<()> {
        // Remove NDP proxy range
        if let Some(ref ndp) = self.ndp_proxy {
            ndp.delete_range(&tap.device_name).await;
        }

        // Remove port forwards for this VM
        let removed_forwards = {
            let mut pf = self.port_forwards.lock().await;
            pf.remove_all_for_vm(vm_id)
        };
        for fwd in &removed_forwards {
            let _ = self.nftables.remove_port_forward(
                fwd.host_port,
                fwd.vm_port,
                fwd.protocol,
            );
        }

        // Tear down nftables rules
        self.nftables.teardown_vm(vm_id)?;

        // Delete TAP interface
        crate::tap::delete_tap(tap).await?;

        info!(vm_id = %vm_id, "VM network destroyed");
        Ok(())
    }

    /// Add a port forwarding rule.
    pub async fn add_port_forward(
        &self,
        vm_id: &str,
        tap: &TapInterface,
        host_port: u16,
        vm_port: u16,
        protocol: Protocol,
    ) -> Result<PortForward> {
        let actual_host_port = if host_port == 0 {
            // Auto-allocate
            let pf = self.port_forwards.lock().await;
            pf.auto_allocate(protocol, 10000)
                .context("no available ports for auto-allocation")?
        } else {
            host_port
        };

        // Check availability
        {
            let pf = self.port_forwards.lock().await;
            if !pf.is_available(actual_host_port, protocol) {
                anyhow::bail!(
                    "port {} ({}) is already in use",
                    actual_host_port,
                    protocol
                );
            }
        }

        // Add nftables rule
        self.nftables
            .add_port_forward(vm_id, tap.guest_ipv4(), actual_host_port, vm_port, protocol)?;

        // Track state
        let forward = PortForward {
            vm_id: vm_id.to_string(),
            host_port: actual_host_port,
            vm_port,
            protocol,
        };

        {
            let mut pf = self.port_forwards.lock().await;
            pf.add(forward.clone());
        }

        Ok(forward)
    }

    /// Remove a port forwarding rule.
    pub async fn remove_port_forward(
        &self,
        host_port: u16,
        protocol: Protocol,
    ) -> Result<()> {
        let forward = {
            let mut pf = self.port_forwards.lock().await;
            pf.remove(host_port, protocol)
                .context("port forward not found")?
        };

        self.nftables
            .remove_port_forward(host_port, forward.vm_port, protocol)?;

        Ok(())
    }

    /// List port forwards (optionally filtered by VM).
    pub async fn list_port_forwards(&self, vm_id: Option<&str>) -> Vec<PortForward> {
        let pf = self.port_forwards.lock().await;
        match vm_id {
            Some(id) => pf.list_for_vm(id).into_iter().cloned().collect(),
            None => pf.list_all().into_iter().cloned().collect(),
        }
    }
}

async fn enable_ipv4_forwarding() -> Result<()> {
    tokio::fs::write("/proc/sys/net/ipv4/ip_forward", "1")
        .await
        .context("failed to enable IPv4 forwarding")?;
    info!("IPv4 forwarding enabled");
    Ok(())
}

async fn enable_ipv6_forwarding() -> Result<()> {
    tokio::fs::write("/proc/sys/net/ipv6/conf/all/forwarding", "1")
        .await
        .context("failed to enable IPv6 forwarding")?;
    info!("IPv6 forwarding enabled");
    Ok(())
}
