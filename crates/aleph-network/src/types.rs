use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::{Ipv4Net, Ipv6Net};
use serde::{Deserialize, Serialize};

/// A TAP interface with dual-stack addressing for a VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapInterface {
    /// Linux interface name (e.g. "vmtap0").
    pub device_name: String,
    /// IPv4 subnet for this VM (e.g. 172.16.0.0/30).
    pub ipv4_network: Ipv4Net,
    /// IPv6 subnet for this VM (e.g. fd00::1/124).
    pub ipv6_network: Ipv6Net,
}

impl TapInterface {
    /// Host-side IPv4 address (first host address in the subnet).
    pub fn host_ipv4(&self) -> Ipv4Addr {
        let net_addr: u32 = self.ipv4_network.network().into();
        Ipv4Addr::from(net_addr + 1)
    }

    /// Guest/VM-side IPv4 address (second host address in the subnet).
    pub fn guest_ipv4(&self) -> Ipv4Addr {
        let net_addr: u32 = self.ipv4_network.network().into();
        Ipv4Addr::from(net_addr + 2)
    }

    /// Host-side IPv6 address (first address in the /124 subnet).
    pub fn host_ipv6(&self) -> Ipv6Addr {
        self.ipv6_network.network()
    }

    /// Guest/VM-side IPv6 address (second address in the /124 subnet).
    pub fn guest_ipv6(&self) -> Ipv6Addr {
        let octets = self.ipv6_network.network().octets();
        let mut addr = u128::from_be_bytes(octets);
        addr += 1;
        Ipv6Addr::from(addr.to_be_bytes())
    }

    /// MAC address derived from the guest IPv4 address.
    /// Format: 52:54:00:{oct2:02x}:{oct3:02x}:{oct4:02x}
    pub fn mac_address(&self) -> String {
        let o = self.guest_ipv4().octets();
        format!("52:54:00:{:02x}:{:02x}:{:02x}", o[1], o[2], o[3])
    }
}

/// Network configuration for the host.
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Host's external network interface (e.g. "eth0").
    pub external_interface: String,
    /// Bridge interface name (e.g. "br-aleph").
    pub bridge: String,
    /// IPv4 address pool for VM subnets.
    pub ipv4_pool: Ipv4Net,
    /// Number of prefix bits per VM subnet (e.g. 30 for /30 subnets).
    pub vm_subnet_prefix: u8,
    /// IPv6 address pool for VM subnets.
    pub ipv6_pool: Ipv6Net,
    /// Whether to enable IPv6 forwarding.
    pub ipv6_enabled: bool,
    /// Whether to use NDP proxy (ndppd).
    pub use_ndp_proxy: bool,
    /// Chain prefix for nftables rules.
    pub nftables_prefix: String,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            external_interface: "eth0".to_string(),
            bridge: "br-aleph".to_string(),
            ipv4_pool: "172.16.0.0/12".parse().unwrap(),
            vm_subnet_prefix: 30,
            ipv6_pool: "fd00::/64".parse().unwrap(),
            ipv6_enabled: true,
            use_ndp_proxy: false,
            nftables_prefix: "aleph".to_string(),
        }
    }
}

/// Port forwarding entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortForward {
    pub vm_id: String,
    pub host_port: u16,
    pub vm_port: u16,
    pub protocol: Protocol,
}

/// Network protocol for port forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Tcp => write!(f, "tcp"),
            Protocol::Udp => write!(f, "udp"),
        }
    }
}

impl std::str::FromStr for Protocol {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tcp" => Ok(Protocol::Tcp),
            "udp" => Ok(Protocol::Udp),
            _ => anyhow::bail!("unknown protocol: {s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tap_interface_ipv4_addresses() {
        let tap = TapInterface {
            device_name: "vmtap0".to_string(),
            ipv4_network: "172.16.0.0/30".parse().unwrap(),
            ipv6_network: "fd00::/124".parse().unwrap(),
        };
        assert_eq!(tap.host_ipv4(), Ipv4Addr::new(172, 16, 0, 1));
        assert_eq!(tap.guest_ipv4(), Ipv4Addr::new(172, 16, 0, 2));
    }

    #[test]
    fn test_tap_interface_ipv6_addresses() {
        let tap = TapInterface {
            device_name: "vmtap0".to_string(),
            ipv4_network: "172.16.0.0/30".parse().unwrap(),
            ipv6_network: "fd00::/124".parse().unwrap(),
        };
        assert_eq!(tap.host_ipv6(), "fd00::".parse::<Ipv6Addr>().unwrap());
        assert_eq!(tap.guest_ipv6(), "fd00::1".parse::<Ipv6Addr>().unwrap());
    }

    #[test]
    fn test_tap_interface_mac_address() {
        let tap = TapInterface {
            device_name: "vmtap0".to_string(),
            ipv4_network: "10.0.100.0/30".parse().unwrap(),
            ipv6_network: "fd00::/124".parse().unwrap(),
        };
        // guest_ipv4 = 10.0.100.2 → 52:54:00:00:64:02
        assert_eq!(tap.mac_address(), "52:54:00:00:64:02");
    }

    #[test]
    fn test_protocol_display_and_parse() {
        assert_eq!(Protocol::Tcp.to_string(), "tcp");
        assert_eq!(Protocol::Udp.to_string(), "udp");
        assert_eq!("tcp".parse::<Protocol>().unwrap(), Protocol::Tcp);
        assert_eq!("UDP".parse::<Protocol>().unwrap(), Protocol::Udp);
        assert!("invalid".parse::<Protocol>().is_err());
    }
}
