# Scheduler Agent SDK/CLI Split — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extract reusable library modules from the `aleph-scheduler-agent` binary crate so the adapter, allocations, volumes, and gRPC client logic are importable as a library.

**Architecture:** Add a `[lib]` target alongside the existing `[[bin]]` in the same crate. Move all domain logic into `lib.rs`-exported public modules. `main.rs` becomes a thin binary that imports from the library. No behavior changes — pure refactor.

**Tech Stack:** Rust, tonic (gRPC), actix-web, aleph-compute-proto

---

### Task 1: Add lib target to Cargo.toml

**Files:**
- Modify: `crates/aleph-scheduler-agent/Cargo.toml`

**Step 1: Add `[lib]` section**

Add a `[lib]` section before the existing `[[bin]]` section:

```toml
[lib]
name = "aleph_scheduler_agent"
path = "src/lib.rs"
```

The file should look like:

```toml
[package]
name = "aleph-scheduler-agent"
version.workspace = true
edition.workspace = true
license.workspace = true

[lib]
name = "aleph_scheduler_agent"
path = "src/lib.rs"

[[bin]]
name = "aleph-scheduler-agent"
path = "src/main.rs"

[dependencies]
# ... unchanged ...
```

**Step 2: Commit**

```bash
git add crates/aleph-scheduler-agent/Cargo.toml
git commit -m "build(scheduler-agent): add lib target to Cargo.toml"
```

---

### Task 2: Create the client module

**Files:**
- Create: `crates/aleph-scheduler-agent/src/client.rs`

**Step 1: Create `client.rs`**

Extract the `connect_compute_node` function from `main.rs` (lines 68-88) into its own module. Make it public:

```rust
//! gRPC client for connecting to the compute-node over a Unix domain socket.

use std::path::Path;

use anyhow::Result;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use aleph_compute_proto::compute::compute_node_client::ComputeNodeClient;

/// Connect to the compute-node gRPC server over a Unix domain socket.
pub async fn connect_compute_node(
    socket_path: &Path,
) -> Result<ComputeNodeClient<Channel>> {
    let socket_path = socket_path.to_path_buf();

    let channel = Endpoint::try_from("http://[::]:0")?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;

    Ok(ComputeNodeClient::new(channel))
}
```

**Step 2: Commit**

```bash
git add crates/aleph-scheduler-agent/src/client.rs
git commit -m "refactor(scheduler-agent): extract client module"
```

---

### Task 3: Create lib.rs

**Files:**
- Create: `crates/aleph-scheduler-agent/src/lib.rs`
- Modify: `crates/aleph-scheduler-agent/src/aleph/mod.rs`

**Step 1: Create `lib.rs`**

```rust
pub mod adapter;
pub mod aleph;
pub mod client;
```

**Step 2: Update `aleph/mod.rs`**

Remove the `#[allow(dead_code)]` attributes — these modules are now public API:

```rust
pub mod allocations;
pub mod messages;
pub mod volumes;
```

**Step 3: Verify it compiles**

Run: `cargo check -p aleph-scheduler-agent`
Expected: compiles with no errors (warnings are OK for now)

**Step 4: Commit**

```bash
git add crates/aleph-scheduler-agent/src/lib.rs crates/aleph-scheduler-agent/src/aleph/mod.rs
git commit -m "refactor(scheduler-agent): create lib.rs exporting public modules"
```

---

### Task 4: Update main.rs to import from the library

**Files:**
- Modify: `crates/aleph-scheduler-agent/src/main.rs`

**Step 1: Replace module declarations and imports**

Remove lines 1-2 (`mod adapter; mod aleph;`) and the `connect_compute_node` function (lines 68-88).

Replace `crate::` imports with `aleph_scheduler_agent::` imports. The top of `main.rs` should become:

```rust
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use anyhow::Context;
use clap::Parser;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use aleph_compute_proto::compute::{DeleteVmRequest, HealthRequest, ListVmsRequest};
use tonic::transport::Channel;

use aleph_scheduler_agent::adapter::{self, AdapterConfig};
use aleph_scheduler_agent::aleph::allocations::{self, Allocation};
use aleph_scheduler_agent::aleph::messages::{ExecutableMessage, ItemHash};
use aleph_scheduler_agent::aleph::volumes::VolumeCache;
use aleph_scheduler_agent::client::connect_compute_node;
```

The rest of `main.rs` (Cli struct, AppState, handlers, main fn) stays exactly the same, except:
- Remove the `connect_compute_node` function definition (it's now imported)
- All handler code that used `crate::` paths now uses the library paths (already handled by the imports above)

**Step 2: Verify it compiles**

Run: `cargo check -p aleph-scheduler-agent`
Expected: compiles with no errors

**Step 3: Run existing tests**

Run: `cargo test -p aleph-scheduler-agent`
Expected: all existing tests pass (adapter::tests, aleph::messages::tests, aleph::allocations::tests, aleph::volumes::tests)

**Step 4: Commit**

```bash
git add crates/aleph-scheduler-agent/src/main.rs
git commit -m "refactor(scheduler-agent): main.rs imports from library crate"
```

---

### Task 5: Verify the library is importable

**Step 1: Run full workspace build**

Run: `cargo build --workspace`
Expected: builds successfully

**Step 2: Run full workspace tests**

Run: `cargo test --workspace`
Expected: all tests pass

**Step 3: Verify lib exports work**

Run: `cargo doc -p aleph-scheduler-agent --no-deps`
Expected: generates docs with public modules `adapter`, `aleph`, `client`
