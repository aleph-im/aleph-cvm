use serde_json::Value;

/// Generate an "add chain" command if the chain doesn't already exist.
pub fn add_chain_if_not_present(
    ruleset: &[Value],
    family: &str,
    table: &str,
    chain_name: &str,
) -> Vec<Value> {
    if chain_exists(ruleset, family, table, chain_name) {
        return vec![];
    }

    vec![serde_json::json!({
        "add": {"chain": {
            "family": family,
            "table": table,
            "name": chain_name,
        }}
    })]
}

/// Check if a chain with the given name exists in the ruleset.
fn chain_exists(ruleset: &[Value], family: &str, table: &str, name: &str) -> bool {
    ruleset.iter().any(|entry| {
        entry
            .get("chain")
            .map(|c| {
                c.get("family").and_then(|f| f.as_str()) == Some(family)
                    && c.get("table").and_then(|t| t.as_str()) == Some(table)
                    && c.get("name").and_then(|n| n.as_str()) == Some(name)
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_chain_exists() {
        let ruleset = vec![json!({
            "chain": {
                "family": "ip",
                "table": "nat",
                "name": "POSTROUTING",
                "handle": 1,
                "type": "nat",
                "hook": "postrouting",
                "prio": 100,
                "policy": "accept"
            }
        })];

        assert!(chain_exists(&ruleset, "ip", "nat", "POSTROUTING"));
        assert!(!chain_exists(&ruleset, "ip", "nat", "nonexistent"));
        assert!(!chain_exists(&ruleset, "ip6", "nat", "POSTROUTING"));
    }

    #[test]
    fn test_add_chain_if_not_present() {
        let ruleset = vec![json!({
            "chain": {
                "family": "ip",
                "table": "nat",
                "name": "existing",
            }
        })];

        // Existing chain → no commands
        assert!(add_chain_if_not_present(&ruleset, "ip", "nat", "existing").is_empty());

        // New chain → one add command
        let cmds = add_chain_if_not_present(&ruleset, "ip", "nat", "new-chain");
        assert_eq!(cmds.len(), 1);
        assert!(cmds[0]["add"]["chain"]["name"].as_str() == Some("new-chain"));
    }
}
