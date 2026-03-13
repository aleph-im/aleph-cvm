# Scheduler Polling Endpoints — Implementation Plan

> **For Claude:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the three GET endpoints the scheduler polls to understand CRN state: system usage, running executions, and node configuration.

**Architecture:** New `status` SDK module with pure types and parsing logic (testable without HTTP/gRPC). Three new actix-web GET handlers in `main.rs`. New `RunArgs` fields for node capabilities (confidential computing, IPv6, payment address).

**Tech Stack:** Rust, actix-web, tonic (gRPC), serde, libc (statvfs)

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/aleph-scheduler-agent/Cargo.toml` | Modify | Add `libc` dependency |
| `crates/aleph-scheduler-agent/src/lib.rs` | Modify | Add `pub mod status` |
| `crates/aleph-scheduler-agent/src/status/mod.rs` | Create | Re-export submodules |
| `crates/aleph-scheduler-agent/src/status/usage.rs` | Create | `MachineUsage` types + `/proc` parsing |
| `crates/aleph-scheduler-agent/src/status/executions.rs` | Create | `ExecutionRecord` + `VmInfo` mapping |
| `crates/aleph-scheduler-agent/src/status/config.rs` | Create | `CrnConfig` type |
| `crates/aleph-scheduler-agent/src/main.rs` | Modify | 3 GET handlers + new `RunArgs` fields |

---

### Task 1: Status module + MachineUsage types and /proc parsing

**Files:**
- Modify: `crates/aleph-scheduler-agent/Cargo.toml`
- Create: `crates/aleph-scheduler-agent/src/status/mod.rs`
- Create: `crates/aleph-scheduler-agent/src/status/usage.rs`
- Modify: `crates/aleph-scheduler-agent/src/lib.rs`

- [ ] **Step 1: Add `libc` dependency**

In `Cargo.toml`, add under `[dependencies]`:

```toml
libc = "0.2"
```

- [ ] **Step 2: Create `status/mod.rs`**

Start with only the `usage` module (others added in their respective tasks):

```rust
pub mod usage;
```

- [ ] **Step 3: Add `pub mod status` to `lib.rs`**

Add after the existing module declarations:

```rust
pub mod status;
```

- [ ] **Step 4: Write failing tests in `status/usage.rs`**

```rust
//! Host resource usage collection from /proc and statvfs.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

// Types and implementation will go here after tests.

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_parse_cpu_count() {
        let cpuinfo = "\
processor\t: 0
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7

processor\t: 1
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7

processor\t: 2
vendor_id\t: GenuineIntel

processor\t: 3
vendor_id\t: GenuineIntel
";
        assert_eq!(parse_cpu_count(cpuinfo), 4);
    }

    #[test]
    fn test_parse_cpu_count_single() {
        let cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\n";
        assert_eq!(parse_cpu_count(cpuinfo), 1);
    }

    #[test]
    fn test_parse_load_average() {
        let loadavg = "0.08 0.03 0.01 1/234 12345\n";
        let result = parse_load_average(loadavg).unwrap();
        assert!((result.load1 - 0.08).abs() < f64::EPSILON);
        assert!((result.load5 - 0.03).abs() < f64::EPSILON);
        assert!((result.load15 - 0.01).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_load_average_high_load() {
        let loadavg = "12.50 8.25 4.00 5/500 99999\n";
        let result = parse_load_average(loadavg).unwrap();
        assert!((result.load1 - 12.50).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_meminfo() {
        let meminfo = "\
MemTotal:       16384000 kB
MemFree:         1234000 kB
MemAvailable:    8192000 kB
Buffers:          456000 kB
";
        let result = parse_meminfo(meminfo).unwrap();
        assert_eq!(result.total_kb, 16384000);
        assert_eq!(result.available_kb, 8192000);
    }

    #[test]
    fn test_parse_meminfo_missing_available() {
        let meminfo = "MemTotal:       16384000 kB\n";
        assert!(parse_meminfo(meminfo).is_err());
    }

    #[test]
    fn test_disk_usage_on_tmp() {
        let usage = disk_usage(Path::new("/tmp")).unwrap();
        assert!(usage.total_kb > 0);
        assert!(usage.available_kb > 0);
        assert!(usage.total_kb >= usage.available_kb);
    }
}
```

- [ ] **Step 5: Run tests to verify they fail**

Run: `cargo test -p aleph-scheduler-agent status::usage`
Expected: compilation errors (types/functions not defined yet)

- [ ] **Step 6: Implement types and parsing functions**

Add above the `#[cfg(test)]` block in `status/usage.rs`:

```rust
//! Host resource usage collection from /proc and statvfs.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct MachineUsage {
    pub cpu: CpuUsage,
    pub mem: MemoryUsage,
    pub disk: DiskUsage,
}

#[derive(Debug, Serialize)]
pub struct CpuUsage {
    pub count: u32,
    pub load_average: LoadAverage,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadAverage {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[derive(Debug, Serialize)]
pub struct MemoryUsage {
    #[serde(rename = "total_kB")]
    pub total_kb: u64,
    #[serde(rename = "available_kB")]
    pub available_kb: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskUsage {
    #[serde(rename = "total_kB")]
    pub total_kb: u64,
    #[serde(rename = "available_kB")]
    pub available_kb: u64,
}

/// Count processors from /proc/cpuinfo content.
pub fn parse_cpu_count(cpuinfo: &str) -> u32 {
    cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count() as u32
}

/// Parse load averages from /proc/loadavg content.
pub fn parse_load_average(loadavg: &str) -> Result<LoadAverage> {
    let parts: Vec<&str> = loadavg.split_whitespace().collect();
    anyhow::ensure!(parts.len() >= 3, "invalid /proc/loadavg format");
    Ok(LoadAverage {
        load1: parts[0].parse().context("load1")?,
        load5: parts[1].parse().context("load5")?,
        load15: parts[2].parse().context("load15")?,
    })
}

/// Parse MemTotal and MemAvailable from /proc/meminfo content.
pub fn parse_meminfo(meminfo: &str) -> Result<MemoryUsage> {
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        if let Some(val) = line.strip_prefix("MemTotal:") {
            total = Some(parse_kb_value(val)?);
        } else if let Some(val) = line.strip_prefix("MemAvailable:") {
            available = Some(parse_kb_value(val)?);
        }
    }
    Ok(MemoryUsage {
        total_kb: total.context("MemTotal not found in /proc/meminfo")?,
        available_kb: available.context("MemAvailable not found in /proc/meminfo")?,
    })
}

fn parse_kb_value(val: &str) -> Result<u64> {
    val.split_whitespace()
        .next()
        .context("empty value")?
        .parse()
        .context("invalid integer")
}

/// Get filesystem usage via statvfs.
pub fn disk_usage(path: &Path) -> Result<DiskUsage> {
    let c_path =
        std::ffi::CString::new(path.to_str().context("non-UTF8 path")?)?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret != 0 {
        anyhow::bail!(
            "statvfs({}): {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(DiskUsage {
        total_kb: stat.f_blocks * stat.f_frsize / 1024,
        available_kb: stat.f_bavail * stat.f_frsize / 1024,
    })
}

/// Collect all system usage metrics.
pub async fn collect_usage(disk_path: &Path) -> Result<MachineUsage> {
    let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
    let loadavg = tokio::fs::read_to_string("/proc/loadavg").await?;
    let meminfo = tokio::fs::read_to_string("/proc/meminfo").await?;

    Ok(MachineUsage {
        cpu: CpuUsage {
            count: parse_cpu_count(&cpuinfo),
            load_average: parse_load_average(&loadavg)?,
        },
        mem: parse_meminfo(&meminfo)?,
        disk: disk_usage(disk_path)?,
    })
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p aleph-scheduler-agent status::usage`
Expected: all 7 tests pass

- [ ] **Step 8: Commit**

```bash
git add crates/aleph-scheduler-agent/Cargo.toml \
       crates/aleph-scheduler-agent/src/lib.rs \
       crates/aleph-scheduler-agent/src/status/
git commit -m "feat(scheduler-agent): add status/usage module with /proc parsing"
```

---

### Task 2: ExecutionRecord type + VmInfo mapping

**Files:**
- Create: `crates/aleph-scheduler-agent/src/status/executions.rs`
- Modify: `crates/aleph-scheduler-agent/src/status/mod.rs`

- [ ] **Step 1: Add `pub mod executions;` to `status/mod.rs`**

- [ ] **Step 2: Write the full file with tests**

```rust
//! Maps compute-node VmInfo to scheduler-facing execution records.

use aleph_compute_proto::compute::VmInfo;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub struct ExecutionRecord {
    pub status: String,
    pub ipv4: String,
    pub ipv6: String,
    pub is_confidential: bool,
    pub uptime_secs: u64,
}

/// Convert a single VmInfo to an ExecutionRecord.
pub fn vm_info_to_execution(vm: &VmInfo) -> ExecutionRecord {
    ExecutionRecord {
        status: vm.status.clone(),
        ipv4: vm.ipv4.clone(),
        ipv6: vm.ipv6.clone(),
        is_confidential: !vm.tee_backend.is_empty(),
        uptime_secs: vm.uptime_secs,
    }
}

/// Map a list of VmInfo into a HashMap keyed by vm_id (which is the ItemHash).
pub fn map_executions(vms: &[VmInfo]) -> HashMap<String, ExecutionRecord> {
    vms.iter()
        .map(|vm| (vm.vm_id.clone(), vm_info_to_execution(vm)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vm(vm_id: &str, tee: &str) -> VmInfo {
        VmInfo {
            vm_id: vm_id.to_string(),
            status: "running".to_string(),
            ipv4: "10.0.200.2".to_string(),
            ipv6: "fd00::2".to_string(),
            tee_backend: tee.to_string(),
            uptime_secs: 3600,
            numa_node: 0,
        }
    }

    #[test]
    fn test_confidential_vm() {
        let vm = make_vm("abc123", "SevSnp");
        let record = vm_info_to_execution(&vm);
        assert_eq!(record.status, "running");
        assert_eq!(record.ipv4, "10.0.200.2");
        assert_eq!(record.ipv6, "fd00::2");
        assert!(record.is_confidential);
        assert_eq!(record.uptime_secs, 3600);
    }

    #[test]
    fn test_non_confidential_vm() {
        let vm = make_vm("xyz789", "");
        let record = vm_info_to_execution(&vm);
        assert!(!record.is_confidential);
    }

    #[test]
    fn test_map_executions() {
        let vms = vec![make_vm("hash1", "SevSnp"), make_vm("hash2", "")];
        let map = map_executions(&vms);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("hash1"));
        assert!(map.contains_key("hash2"));
        assert!(map["hash1"].is_confidential);
        assert!(!map["hash2"].is_confidential);
    }

    #[test]
    fn test_map_executions_empty() {
        let map = map_executions(&[]);
        assert!(map.is_empty());
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p aleph-scheduler-agent status::executions`
Expected: all 4 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/aleph-scheduler-agent/src/status/executions.rs \
       crates/aleph-scheduler-agent/src/status/mod.rs
git commit -m "feat(scheduler-agent): add status/executions module"
```

---

### Task 3: CrnConfig type

**Files:**
- Create: `crates/aleph-scheduler-agent/src/status/config.rs`
- Modify: `crates/aleph-scheduler-agent/src/status/mod.rs`

- [ ] **Step 1: Add `pub mod config;` to `status/mod.rs`**

- [ ] **Step 2: Write the full file with tests**

```rust
//! CRN configuration reported to the scheduler.

use serde::Serialize;

/// Node capabilities and configuration, served at GET /status/config.
#[derive(Debug, Clone, Serialize)]
pub struct CrnConfig {
    pub enable_confidential_computing: bool,
    pub ipv6_support: bool,
    pub gpu_support: bool,
    pub payment_receiver_address: Option<String>,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialization_full() {
        let config = CrnConfig {
            enable_confidential_computing: true,
            ipv6_support: true,
            gpu_support: false,
            payment_receiver_address: Some("0x1234abcd".to_string()),
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["enable_confidential_computing"], true);
        assert_eq!(json["ipv6_support"], true);
        assert_eq!(json["gpu_support"], false);
        assert_eq!(json["payment_receiver_address"], "0x1234abcd");
        assert_eq!(json["version"], "0.1.0");
    }

    #[test]
    fn test_serialization_no_payment() {
        let config = CrnConfig {
            enable_confidential_computing: false,
            ipv6_support: false,
            gpu_support: false,
            payment_receiver_address: None,
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert!(json["payment_receiver_address"].is_null());
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p aleph-scheduler-agent status::config`
Expected: all 2 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/aleph-scheduler-agent/src/status/config.rs \
       crates/aleph-scheduler-agent/src/status/mod.rs
git commit -m "feat(scheduler-agent): add status/config module"
```

---

### Task 4: Wire up GET endpoints + new RunArgs fields

**Files:**
- Modify: `crates/aleph-scheduler-agent/src/main.rs`

- [ ] **Step 1: Add imports**

Add to the existing `use aleph_scheduler_agent::` imports in `main.rs`:

```rust
use aleph_scheduler_agent::status::config::CrnConfig;
use aleph_scheduler_agent::status::executions::map_executions;
use aleph_scheduler_agent::status::usage::collect_usage;
```

- [ ] **Step 2: Add new RunArgs fields**

Add these fields to the `RunArgs` struct (after the existing fields):

```rust
    /// Enable confidential computing (SEV-SNP).
    #[arg(long, env = "ALEPH_VM_ENABLE_CONFIDENTIAL_COMPUTING")]
    enable_confidential_computing: bool,

    /// IPv6 address pool (non-empty value indicates IPv6 support).
    #[arg(long, env = "ALEPH_VM_IPV6_ADDRESS_POOL")]
    ipv6_address_pool: Option<String>,

    /// Payment receiver Ethereum address.
    #[arg(long, env = "ALEPH_VM_PAYMENT_RECEIVER_ADDRESS")]
    payment_receiver_address: Option<String>,
```

- [ ] **Step 3: Add fields to AppState**

Add to the `AppState` struct:

```rust
    crn_config: CrnConfig,
    cache_dir: PathBuf,
```

- [ ] **Step 4: Construct CrnConfig and set cache_dir in main()**

In the `Cli::Run(args)` branch, **before** the `Arc::new(AppState { ... })` block, clone `cache_dir` (it's moved into `VolumeCache::new` later) and build `CrnConfig`:

```rust
            let cache_dir = args.cache_dir.clone();
            let crn_config = CrnConfig {
                enable_confidential_computing: args.enable_confidential_computing,
                ipv6_support: args.ipv6_address_pool.is_some(),
                gpu_support: false,
                payment_receiver_address: args.payment_receiver_address.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            };
```

Then pass `crn_config` and `cache_dir` into the `AppState` struct literal.

- [ ] **Step 5: Add three handler functions**

Add after the existing handlers. Note: signatures use `Arc<AppState>` to match the existing handler pattern:

```rust
async fn get_system_usage(state: web::Data<Arc<AppState>>) -> HttpResponse {
    match collect_usage(&state.cache_dir).await {
        Ok(usage) => HttpResponse::Ok().json(usage),
        Err(e) => {
            error!("Failed to collect system usage: {e:#}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

async fn get_executions(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let mut client = state.compute_client.write().await;
    match client.list_vms(ListVmsRequest {}).await {
        Ok(resp) => {
            let executions = map_executions(&resp.into_inner().vms);
            HttpResponse::Ok().json(executions)
        }
        Err(e) => {
            error!("Failed to list VMs: {e:#}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

async fn get_config(state: web::Data<Arc<AppState>>) -> HttpResponse {
    HttpResponse::Ok().json(&state.crn_config)
}
```

- [ ] **Step 6: Register routes**

Add to the `App::new()` builder alongside the existing routes:

```rust
                    .route("/about/usage/system", web::get().to(get_system_usage))
                    .route("/about/executions/list", web::get().to(get_executions))
                    .route("/status/config", web::get().to(get_config))
```

- [ ] **Step 7: Verify compilation**

Run: `cargo check -p aleph-scheduler-agent`
Expected: compiles with no errors

- [ ] **Step 8: Commit**

```bash
git add crates/aleph-scheduler-agent/src/main.rs
git commit -m "feat(scheduler-agent): wire up scheduler polling GET endpoints"
```

---

### Task 5: Final verification

- [ ] **Step 1: Build the workspace**

Run: `cargo build --workspace`
Expected: builds successfully

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: all tests pass (including the 13 new tests from the status module)

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings
