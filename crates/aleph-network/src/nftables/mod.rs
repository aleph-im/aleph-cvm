mod chains;
mod rules;

use std::net::Ipv4Addr;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::types::Protocol;

/// Manages nftables rules for VM networking.
///
/// Mirrors the chain structure from aleph-vm's firewall.py:
/// - Supervisor chains: `{prefix}-supervisor-nat`, `{prefix}-supervisor-filter`,
///   `{prefix}-supervisor-prerouting`
/// - Per-VM chains: `{prefix}-vm-nat-{id}`, `{prefix}-vm-filter-{id}`
pub struct NftablesManager {
    prefix: String,
    external_interface: String,
    ipv6_enabled: bool,
}

impl NftablesManager {
    pub fn new(prefix: &str, external_interface: &str, ipv6_enabled: bool) -> Self {
        Self {
            prefix: prefix.to_string(),
            external_interface: external_interface.to_string(),
            ipv6_enabled,
        }
    }

    // ─── Chain names ────────────────────────────────────────────────────────

    fn supervisor_nat_chain(&self) -> String {
        format!("{}-supervisor-nat", self.prefix)
    }

    fn supervisor_filter_chain(&self) -> String {
        format!("{}-supervisor-filter", self.prefix)
    }

    fn supervisor_prerouting_chain(&self) -> String {
        format!("{}-supervisor-prerouting", self.prefix)
    }

    fn vm_nat_chain(&self, vm_id: &str) -> String {
        format!("{}-vm-nat-{}", self.prefix, vm_id)
    }

    fn vm_filter_chain(&self, vm_id: &str) -> String {
        format!("{}-vm-filter-{}", self.prefix, vm_id)
    }

    // ─── Lifecycle ──────────────────────────────────────────────────────────

    /// Initialize all supervisor chains and base rules.
    pub fn initialize(&self) -> Result<()> {
        info!(prefix = %self.prefix, "initializing nftables");

        // Get existing ruleset to find base chain tables
        let ruleset = execute_list_ruleset()?;

        // Find or create base chains and their tables
        let nat_table = find_or_create_table_for_hook(&ruleset, "postrouting", "ip")?;
        let filter_table = find_or_create_table_for_hook(&ruleset, "forward", "ip")?;
        let prerouting_table = find_or_create_table_for_hook(&ruleset, "prerouting", "ip")?;

        // Re-fetch after potential base chain creation
        let ruleset = execute_list_ruleset()?;

        let mut commands = Vec::new();

        // Create supervisor-nat chain + jump from postrouting
        let sup_nat = self.supervisor_nat_chain();
        commands.extend(chains::add_chain_if_not_present(
            &ruleset, "ip", &nat_table, &sup_nat,
        ));
        commands.extend(rules::add_jump_if_not_present(
            &ruleset, "ip", &nat_table, &self.find_base_chain_name(&ruleset, "ip", &nat_table, "postrouting"),
            &sup_nat,
        ));

        // Create supervisor-filter chain + jump from forward + conntrack rule
        let sup_filter = self.supervisor_filter_chain();
        commands.extend(chains::add_chain_if_not_present(
            &ruleset, "ip", &filter_table, &sup_filter,
        ));
        commands.extend(rules::add_jump_if_not_present(
            &ruleset, "ip", &filter_table,
            &self.find_base_chain_name(&ruleset, "ip", &filter_table, "forward"),
            &sup_filter,
        ));
        commands.extend(rules::add_conntrack_if_not_present(
            &ruleset, "ip", &filter_table, &sup_filter,
        ));

        // Create supervisor-prerouting chain + jump from prerouting
        let sup_pre = self.supervisor_prerouting_chain();
        commands.extend(chains::add_chain_if_not_present(
            &ruleset, "ip", &prerouting_table, &sup_pre,
        ));
        commands.extend(rules::add_jump_if_not_present(
            &ruleset, "ip", &prerouting_table,
            &self.find_base_chain_name(&ruleset, "ip", &prerouting_table, "prerouting"),
            &sup_pre,
        ));

        // IPv6 forwarding
        if self.ipv6_enabled {
            let ip6_filter_table = find_or_create_table_for_hook(&execute_list_ruleset()?, "forward", "ip6")?;
            let ruleset = execute_list_ruleset()?;
            let sup_filter6 = self.supervisor_filter_chain();
            commands.extend(chains::add_chain_if_not_present(
                &ruleset, "ip6", &ip6_filter_table, &sup_filter6,
            ));
            commands.extend(rules::add_jump_if_not_present(
                &ruleset, "ip6", &ip6_filter_table,
                &self.find_base_chain_name(&ruleset, "ip6", &ip6_filter_table, "forward"),
                &sup_filter6,
            ));
            commands.extend(rules::add_conntrack_if_not_present(
                &ruleset, "ip6", &ip6_filter_table, &sup_filter6,
            ));
        }

        if !commands.is_empty() {
            execute_nft_commands(&commands)?;
        }

        info!("nftables initialized");
        Ok(())
    }

    /// Remove all supervisor chains and their jump rules.
    pub fn teardown(&self) -> Result<()> {
        info!(prefix = %self.prefix, "tearing down nftables");

        self.remove_chain_everywhere(&self.supervisor_nat_chain())?;
        self.remove_chain_everywhere(&self.supervisor_filter_chain())?;
        self.remove_chain_everywhere(&self.supervisor_prerouting_chain())?;

        Ok(())
    }

    // ─── Per-VM setup ───────────────────────────────────────────────────────

    /// Set up nftables chains and rules for a VM.
    pub fn setup_vm(&self, vm_id: &str, tap_device: &str) -> Result<()> {
        let ruleset = execute_list_ruleset()?;
        let mut commands = Vec::new();

        // Find tables
        let nat_table = get_table_for_chain(&ruleset, "ip", &self.supervisor_nat_chain())
            .context("supervisor nat chain not found — call initialize() first")?;
        let filter_table = get_table_for_chain(&ruleset, "ip", &self.supervisor_filter_chain())
            .context("supervisor filter chain not found — call initialize() first")?;

        // Per-VM NAT chain + jump + masquerade rule
        let vm_nat = self.vm_nat_chain(vm_id);
        commands.extend(chains::add_chain_if_not_present(&ruleset, "ip", &nat_table, &vm_nat));
        commands.extend(rules::add_jump_if_not_present(
            &ruleset, "ip", &nat_table, &self.supervisor_nat_chain(), &vm_nat,
        ));
        commands.extend(rules::add_masquerade_if_not_present(
            &ruleset, "ip", &nat_table, &vm_nat,
            tap_device, &self.external_interface,
        ));

        // Per-VM filter chain + jump + forward-to-external rule
        let vm_filter = self.vm_filter_chain(vm_id);
        commands.extend(chains::add_chain_if_not_present(&ruleset, "ip", &filter_table, &vm_filter));
        commands.extend(rules::add_jump_if_not_present(
            &ruleset, "ip", &filter_table, &self.supervisor_filter_chain(), &vm_filter,
        ));
        commands.extend(rules::add_forward_accept_if_not_present(
            &ruleset, "ip", &filter_table, &vm_filter,
            tap_device, &self.external_interface,
        ));

        // IPv6 forwarding for this VM
        if self.ipv6_enabled {
            if let Some(ip6_filter_table) = get_table_for_chain(&ruleset, "ip6", &self.supervisor_filter_chain()) {
                let vm_filter6 = self.vm_filter_chain(vm_id);
                commands.extend(chains::add_chain_if_not_present(&ruleset, "ip6", &ip6_filter_table, &vm_filter6));
                commands.extend(rules::add_jump_if_not_present(
                    &ruleset, "ip6", &ip6_filter_table, &self.supervisor_filter_chain(), &vm_filter6,
                ));
                commands.extend(rules::add_forward_accept_if_not_present(
                    &ruleset, "ip6", &ip6_filter_table, &vm_filter6,
                    tap_device, &self.external_interface,
                ));
            }
        }

        if !commands.is_empty() {
            execute_nft_commands(&commands)?;
        }

        info!(vm_id = %vm_id, tap = %tap_device, "nftables rules set up for VM");
        Ok(())
    }

    /// Remove all nftables chains and rules for a VM.
    pub fn teardown_vm(&self, vm_id: &str) -> Result<()> {
        self.remove_chain_everywhere(&self.vm_nat_chain(vm_id))?;
        self.remove_chain_everywhere(&self.vm_filter_chain(vm_id))?;
        info!(vm_id = %vm_id, "nftables rules removed for VM");
        Ok(())
    }

    // ─── Port forwarding ────────────────────────────────────────────────────

    /// Add a port forwarding rule (DNAT + accept).
    pub fn add_port_forward(
        &self,
        vm_id: &str,
        guest_ip: Ipv4Addr,
        host_port: u16,
        vm_port: u16,
        protocol: Protocol,
    ) -> Result<()> {
        let ruleset = execute_list_ruleset()?;

        let prerouting_table = get_table_for_chain(&ruleset, "ip", &self.supervisor_prerouting_chain())
            .context("supervisor prerouting chain not found")?;
        let filter_table = get_table_for_chain(&ruleset, "ip", &self.vm_filter_chain(vm_id))
            .context("VM filter chain not found — set up VM first")?;

        let mut commands = Vec::new();

        // DNAT rule in supervisor-prerouting
        commands.push(rules::dnat_rule(
            "ip",
            &prerouting_table,
            &self.supervisor_prerouting_chain(),
            &self.external_interface,
            host_port,
            &guest_ip.to_string(),
            vm_port,
            protocol,
        ));

        // Accept rule in vm-filter chain
        commands.push(rules::port_accept_rule(
            "ip",
            &filter_table,
            &self.vm_filter_chain(vm_id),
            &self.external_interface,
            vm_port,
            protocol,
        ));

        execute_nft_commands(&commands)?;

        info!(
            vm_id = %vm_id, host_port, vm_port, protocol = %protocol,
            "port forwarding rule added"
        );
        Ok(())
    }

    /// Remove a port forwarding rule.
    pub fn remove_port_forward(
        &self,
        host_port: u16,
        vm_port: u16,
        protocol: Protocol,
    ) -> Result<()> {
        let ruleset = execute_list_ruleset()?;
        let mut commands = Vec::new();

        // Find and delete the DNAT rule in supervisor-prerouting
        for entry in &ruleset {
            if let Some(rule) = entry.get("rule") {
                if rules::is_dnat_rule_matching(
                    rule,
                    &self.supervisor_prerouting_chain(),
                    host_port,
                    vm_port,
                    protocol,
                ) {
                    if let Some(handle) = rule.get("handle") {
                        let table = rule["table"].as_str().unwrap_or("");
                        let family = rule["family"].as_str().unwrap_or("ip");
                        commands.push(serde_json::json!({
                            "delete": {"rule": {
                                "family": family,
                                "table": table,
                                "chain": self.supervisor_prerouting_chain(),
                                "handle": handle,
                            }}
                        }));
                    }
                }
            }
        }

        if !commands.is_empty() {
            execute_nft_commands(&commands)?;
            info!(host_port, vm_port, protocol = %protocol, "port forwarding rule removed");
        } else {
            debug!(host_port, vm_port, protocol = %protocol, "no matching port forward rule found");
        }

        Ok(())
    }

    /// Check if a port is already in use by any forwarding rule.
    pub fn is_port_in_use(&self, port: u16) -> Result<bool> {
        let ruleset = execute_list_ruleset()?;
        Ok(rules::port_in_use(&ruleset, port))
    }

    // ─── Helpers ────────────────────────────────────────────────────────────

    /// Remove a chain and all jump rules pointing to it, across all families.
    fn remove_chain_everywhere(&self, chain_name: &str) -> Result<()> {
        let ruleset = execute_list_ruleset()?;
        let mut commands = Vec::new();

        // Find all jump rules targeting this chain and delete them (by handle)
        for entry in &ruleset {
            if let Some(rule) = entry.get("rule") {
                if rules::rule_jumps_to(rule, chain_name) {
                    if let Some(handle) = rule.get("handle") {
                        let family = rule["family"].as_str().unwrap_or("ip");
                        let table = rule["table"].as_str().unwrap_or("");
                        let chain = rule["chain"].as_str().unwrap_or("");
                        commands.push(serde_json::json!({
                            "delete": {"rule": {
                                "family": family,
                                "table": table,
                                "chain": chain,
                                "handle": handle,
                            }}
                        }));
                    }
                }
            }
        }

        // Delete the chains themselves
        for entry in &ruleset {
            if let Some(chain) = entry.get("chain") {
                if chain.get("name").and_then(|n| n.as_str()) == Some(chain_name) {
                    let family = chain["family"].as_str().unwrap_or("ip");
                    let table = chain["table"].as_str().unwrap_or("");
                    commands.push(serde_json::json!({
                        "delete": {"chain": {
                            "family": family,
                            "table": table,
                            "name": chain_name,
                        }}
                    }));
                }
            }
        }

        if !commands.is_empty() {
            execute_nft_commands(&commands)?;
        }

        Ok(())
    }

    /// Find the name of a base chain for a given hook in a table.
    fn find_base_chain_name(&self, ruleset: &[Value], family: &str, table: &str, hook: &str) -> String {
        for entry in ruleset {
            if let Some(chain) = entry.get("chain") {
                if chain.get("family").and_then(|f| f.as_str()) == Some(family)
                    && chain.get("table").and_then(|t| t.as_str()) == Some(table)
                    && chain.get("hook").and_then(|h| h.as_str()) == Some(hook)
                {
                    if let Some(name) = chain.get("name").and_then(|n| n.as_str()) {
                        return name.to_string();
                    }
                }
            }
        }
        // Fallback to the hook name itself (common convention)
        hook.to_uppercase()
    }
}

// ─── nftables execution helpers ─────────────────────────────────────────────

/// Execute `nft -j list ruleset` and return the parsed entries.
fn execute_list_ruleset() -> Result<Vec<Value>> {
    let mut all_entries = Vec::new();

    for family in &["ip", "ip6"] {
        let output = std::process::Command::new("nft")
            .args(["-j", "list", "ruleset", family])
            .output()
            .context("failed to execute nft")?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(parsed) = serde_json::from_str::<Value>(&stdout) {
                if let Some(entries) = parsed.get("nftables").and_then(|n| n.as_array()) {
                    all_entries.extend(entries.iter().cloned());
                }
            }
        }
    }

    Ok(all_entries)
}

/// Execute a batch of nftables JSON commands.
fn execute_nft_commands(commands: &[Value]) -> Result<()> {
    if commands.is_empty() {
        return Ok(());
    }

    let batch = serde_json::json!({"nftables": commands});
    let json = serde_json::to_string(&batch)?;

    debug!(commands = %json, "executing nft commands");

    let mut child = std::process::Command::new("nft")
        .args(["-j", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn nft")?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin.write_all(json.as_bytes())?;
    }

    let output = child.wait_with_output().context("failed to wait for nft")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(stderr = %stderr, "nft command failed");
        anyhow::bail!("nft command failed: {}", stderr.trim());
    }

    Ok(())
}

/// Find the table name for a given hook, creating base chain if needed.
fn find_or_create_table_for_hook(ruleset: &[Value], hook: &str, family: &str) -> Result<String> {
    // Search for an existing base chain with this hook
    for entry in ruleset {
        if let Some(chain) = entry.get("chain") {
            if chain.get("family").and_then(|f| f.as_str()) == Some(family)
                && chain.get("hook").and_then(|h| h.as_str()) == Some(hook)
            {
                if let Some(table) = chain.get("table").and_then(|t| t.as_str()) {
                    return Ok(table.to_string());
                }
            }
        }
    }

    // No base chain found — create a default one
    let (table, chain_type, priority) = match hook {
        "postrouting" => ("nat", "nat", 100),
        "prerouting" => ("nat", "nat", -100),
        "forward" => ("filter", "filter", 0),
        _ => anyhow::bail!("unsupported hook: {hook}"),
    };

    let chain_name = hook.to_uppercase();
    let commands = vec![
        serde_json::json!({
            "add": {"table": {"family": family, "name": table}}
        }),
        serde_json::json!({
            "add": {"chain": {
                "family": family,
                "table": table,
                "name": chain_name,
                "type": chain_type,
                "hook": hook,
                "prio": priority,
                "policy": "accept",
            }}
        }),
    ];

    execute_nft_commands(&commands)?;
    info!(family, table, chain = chain_name, hook, "created base chain");

    Ok(table.to_string())
}

/// Find the table that contains a given chain name.
fn get_table_for_chain(ruleset: &[Value], family: &str, chain_name: &str) -> Option<String> {
    for entry in ruleset {
        if let Some(chain) = entry.get("chain") {
            if chain.get("family").and_then(|f| f.as_str()) == Some(family)
                && chain.get("name").and_then(|n| n.as_str()) == Some(chain_name)
            {
                return chain.get("table").and_then(|t| t.as_str()).map(|s| s.to_string());
            }
        }
    }
    None
}
