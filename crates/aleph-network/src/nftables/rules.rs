use serde_json::Value;

use crate::types::Protocol;

/// Generate a jump rule if one doesn't already exist.
pub fn add_jump_if_not_present(
    ruleset: &[Value],
    family: &str,
    table: &str,
    from_chain: &str,
    to_chain: &str,
) -> Vec<Value> {
    // Check if a jump from from_chain → to_chain already exists
    let exists = ruleset.iter().any(|entry| {
        entry
            .get("rule")
            .map(|r| {
                r.get("family").and_then(|f| f.as_str()) == Some(family)
                    && r.get("table").and_then(|t| t.as_str()) == Some(table)
                    && r.get("chain").and_then(|c| c.as_str()) == Some(from_chain)
                    && has_jump_to(r, to_chain)
            })
            .unwrap_or(false)
    });

    if exists {
        return vec![];
    }

    vec![serde_json::json!({
        "add": {"rule": {
            "family": family,
            "table": table,
            "chain": from_chain,
            "expr": [{"jump": {"target": to_chain}}],
        }}
    })]
}

/// Generate a conntrack "ct state established,related accept" rule if not present.
pub fn add_conntrack_if_not_present(
    ruleset: &[Value],
    family: &str,
    table: &str,
    chain: &str,
) -> Vec<Value> {
    let exists = ruleset.iter().any(|entry| {
        entry
            .get("rule")
            .map(|r| {
                r.get("family").and_then(|f| f.as_str()) == Some(family)
                    && r.get("table").and_then(|t| t.as_str()) == Some(table)
                    && r.get("chain").and_then(|c| c.as_str()) == Some(chain)
                    && has_conntrack_accept(r)
            })
            .unwrap_or(false)
    });

    if exists {
        return vec![];
    }

    vec![serde_json::json!({
        "add": {"rule": {
            "family": family,
            "table": table,
            "chain": chain,
            "expr": [
                {"match": {
                    "op": "in",
                    "left": {"ct": {"key": "state"}},
                    "right": ["established", "related"],
                }},
                {"accept": null},
            ],
        }}
    })]
}

/// Generate a masquerade rule for VM traffic if not present.
pub fn add_masquerade_if_not_present(
    ruleset: &[Value],
    family: &str,
    table: &str,
    chain: &str,
    iifname: &str,
    oifname: &str,
) -> Vec<Value> {
    let exists = ruleset.iter().any(|entry| {
        entry
            .get("rule")
            .map(|r| {
                r.get("family").and_then(|f| f.as_str()) == Some(family)
                    && r.get("table").and_then(|t| t.as_str()) == Some(table)
                    && r.get("chain").and_then(|c| c.as_str()) == Some(chain)
                    && has_masquerade(r)
            })
            .unwrap_or(false)
    });

    if exists {
        return vec![];
    }

    vec![serde_json::json!({
        "add": {"rule": {
            "family": family,
            "table": table,
            "chain": chain,
            "expr": [
                {"match": {
                    "op": "==",
                    "left": {"meta": {"key": "iifname"}},
                    "right": iifname,
                }},
                {"match": {
                    "op": "==",
                    "left": {"meta": {"key": "oifname"}},
                    "right": oifname,
                }},
                {"masquerade": null},
            ],
        }}
    })]
}

/// Generate a forward accept rule for VM → external traffic.
pub fn add_forward_accept_if_not_present(
    ruleset: &[Value],
    family: &str,
    table: &str,
    chain: &str,
    iifname: &str,
    oifname: &str,
) -> Vec<Value> {
    let exists = ruleset.iter().any(|entry| {
        entry
            .get("rule")
            .map(|r| {
                r.get("family").and_then(|f| f.as_str()) == Some(family)
                    && r.get("table").and_then(|t| t.as_str()) == Some(table)
                    && r.get("chain").and_then(|c| c.as_str()) == Some(chain)
                    && has_iifname_oifname_accept(r, iifname, oifname)
            })
            .unwrap_or(false)
    });

    if exists {
        return vec![];
    }

    vec![serde_json::json!({
        "add": {"rule": {
            "family": family,
            "table": table,
            "chain": chain,
            "expr": [
                {"match": {
                    "op": "==",
                    "left": {"meta": {"key": "iifname"}},
                    "right": iifname,
                }},
                {"match": {
                    "op": "==",
                    "left": {"meta": {"key": "oifname"}},
                    "right": oifname,
                }},
                {"accept": null},
            ],
        }}
    })]
}

/// Build a DNAT rule for port forwarding.
///
/// No `iifname` match — the rule applies to traffic from any interface
/// (external, bridge, loopback). The host port is unique per forward,
/// so there's no ambiguity. This allows testing from the host itself
/// and forwarding from both external and bridge-originated traffic.
pub fn dnat_rule(
    family: &str,
    table: &str,
    chain: &str,
    host_port: u16,
    dest_ip: &str,
    dest_port: u16,
    protocol: Protocol,
) -> Value {
    serde_json::json!({
        "add": {"rule": {
            "family": family,
            "table": table,
            "chain": chain,
            "expr": [
                {"match": {
                    "op": "==",
                    "left": {"payload": {"protocol": protocol.to_string(), "field": "dport"}},
                    "right": host_port,
                }},
                {"dnat": {
                    "addr": dest_ip,
                    "port": dest_port,
                }},
            ],
        }}
    })
}

/// Build an accept rule for port forwarding (in the VM's filter chain).
/// Build an accept rule for a forwarded port in the VM's filter chain.
///
/// No `iifname` match — accepts forwarded traffic from any interface,
/// consistent with the DNAT rule.
pub fn port_accept_rule(
    family: &str,
    table: &str,
    chain: &str,
    port: u16,
    protocol: Protocol,
) -> Value {
    serde_json::json!({
        "add": {"rule": {
            "family": family,
            "table": table,
            "chain": chain,
            "expr": [
                {"match": {
                    "op": "==",
                    "left": {"payload": {"protocol": protocol.to_string(), "field": "dport"}},
                    "right": port,
                }},
                {"accept": null},
            ],
        }}
    })
}

/// Check if a rule jumps to the given target chain.
pub fn rule_jumps_to(rule: &Value, target: &str) -> bool {
    if let Some(exprs) = rule.get("expr").and_then(|e| e.as_array()) {
        for expr in exprs {
            if let Some(jump) = expr.get("jump")
                && jump.get("target").and_then(|t| t.as_str()) == Some(target)
            {
                return true;
            }
        }
    }
    false
}

/// Check if a DNAT rule matches the given parameters.
pub fn is_dnat_rule_matching(
    rule: &Value,
    chain: &str,
    host_port: u16,
    _vm_port: u16,
    protocol: Protocol,
) -> bool {
    if rule.get("chain").and_then(|c| c.as_str()) != Some(chain) {
        return false;
    }

    let exprs = match rule.get("expr").and_then(|e| e.as_array()) {
        Some(e) => e,
        None => return false,
    };

    let mut has_dport_match = false;
    let mut has_dnat = false;

    for expr in exprs {
        // Check dport match
        if let Some(m) = expr.get("match")
            && let Some(left) = m.get("left")
            && let Some(payload) = left.get("payload")
            && payload.get("field").and_then(|f| f.as_str()) == Some("dport")
            && payload.get("protocol").and_then(|p| p.as_str()) == Some(&protocol.to_string())
            && m.get("right").and_then(|r| r.as_u64()) == Some(host_port as u64)
        {
            has_dport_match = true;
        }

        // Check dnat
        if expr.get("dnat").is_some() {
            has_dnat = true;
        }
    }

    has_dport_match && has_dnat
}

/// Check if any rule in the ruleset uses a given port.
pub fn port_in_use(ruleset: &[Value], port: u16) -> bool {
    for entry in ruleset {
        if let Some(rule) = entry.get("rule")
            && let Some(exprs) = rule.get("expr").and_then(|e| e.as_array())
        {
            for expr in exprs {
                // Check dport match
                if let Some(m) = expr.get("match")
                    && let Some(right) = m.get("right")
                    && right.as_u64() == Some(port as u64)
                    && let Some(left) = m.get("left")
                    && let Some(payload) = left.get("payload")
                    && payload.get("field").and_then(|f| f.as_str()) == Some("dport")
                {
                    return true;
                }
                // Check dnat port
                if let Some(dnat) = expr.get("dnat")
                    && dnat.get("port").and_then(|p| p.as_u64()) == Some(port as u64)
                {
                    return true;
                }
            }
        }
    }
    false
}

// ─── Helpers for pattern matching ───────────────────────────────────────────

fn has_jump_to(rule: &Value, target: &str) -> bool {
    rule_jumps_to(rule, target)
}

fn has_conntrack_accept(rule: &Value) -> bool {
    if let Some(exprs) = rule.get("expr").and_then(|e| e.as_array()) {
        let has_ct = exprs.iter().any(|e| {
            e.get("match")
                .and_then(|m| m.get("left"))
                .and_then(|l| l.get("ct"))
                .and_then(|ct| ct.get("key"))
                .and_then(|k| k.as_str())
                == Some("state")
        });
        let has_accept = exprs.iter().any(|e| e.get("accept").is_some());
        has_ct && has_accept
    } else {
        false
    }
}

fn has_masquerade(rule: &Value) -> bool {
    if let Some(exprs) = rule.get("expr").and_then(|e| e.as_array()) {
        exprs.iter().any(|e| e.get("masquerade").is_some())
    } else {
        false
    }
}

fn has_iifname_oifname_accept(rule: &Value, iifname: &str, oifname: &str) -> bool {
    if let Some(exprs) = rule.get("expr").and_then(|e| e.as_array()) {
        let mut found_iif = false;
        let mut found_oif = false;
        let mut found_accept = false;

        for e in exprs {
            if let Some(m) = e.get("match")
                && let Some(left) = m.get("left")
                && let Some(meta) = left.get("meta")
            {
                if meta.get("key").and_then(|k| k.as_str()) == Some("iifname")
                    && m.get("right").and_then(|r| r.as_str()) == Some(iifname)
                {
                    found_iif = true;
                }
                if meta.get("key").and_then(|k| k.as_str()) == Some("oifname")
                    && m.get("right").and_then(|r| r.as_str()) == Some(oifname)
                {
                    found_oif = true;
                }
            }
            if e.get("accept").is_some() {
                found_accept = true;
            }
        }

        found_iif && found_oif && found_accept
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_rule_jumps_to() {
        let rule = json!({
            "family": "ip",
            "table": "nat",
            "chain": "POSTROUTING",
            "expr": [{"jump": {"target": "aleph-supervisor-nat"}}],
        });
        assert!(rule_jumps_to(&rule, "aleph-supervisor-nat"));
        assert!(!rule_jumps_to(&rule, "other-chain"));
    }

    #[test]
    fn test_port_in_use() {
        let ruleset = vec![json!({
            "rule": {
                "family": "ip",
                "table": "nat",
                "chain": "aleph-supervisor-prerouting",
                "expr": [
                    {"match": {
                        "op": "==",
                        "left": {"payload": {"protocol": "tcp", "field": "dport"}},
                        "right": 8080,
                    }},
                    {"dnat": {"addr": "172.16.0.2", "port": 80}},
                ],
            }
        })];

        assert!(port_in_use(&ruleset, 8080));
        assert!(port_in_use(&ruleset, 80)); // via dnat port
        assert!(!port_in_use(&ruleset, 9090));
    }

    #[test]
    fn test_is_dnat_rule_matching() {
        let rule = json!({
            "chain": "aleph-supervisor-prerouting",
            "expr": [
                {"match": {
                    "op": "==",
                    "left": {"payload": {"protocol": "tcp", "field": "dport"}},
                    "right": 8080,
                }},
                {"dnat": {"addr": "172.16.0.2", "port": 80}},
            ],
        });

        assert!(is_dnat_rule_matching(
            &rule,
            "aleph-supervisor-prerouting",
            8080,
            80,
            Protocol::Tcp
        ));
        assert!(!is_dnat_rule_matching(
            &rule,
            "aleph-supervisor-prerouting",
            9090,
            80,
            Protocol::Tcp
        ));
        assert!(!is_dnat_rule_matching(
            &rule,
            "other-chain",
            8080,
            80,
            Protocol::Tcp
        ));
    }
}
