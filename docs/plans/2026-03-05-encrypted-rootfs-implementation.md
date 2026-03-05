# Encrypted Rootfs Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Support user-provided LUKS-encrypted rootfs images with decryption key delivered over the attested TLS channel.

**Architecture:** Two mutually exclusive boot modes selected by kernel cmdline: `roothash=<hash>` (dm-verity, existing) or `luks=1` (LUKS, new). In LUKS mode, the attest-agent starts before rootfs mount and exposes a `POST /confidential/inject-secret` endpoint. The user verifies attestation, sends the LUKS passphrase over TLS, init.sh unlocks and mounts.

**Tech Stack:** Rust (actix-web, reqwest, clap), shell (busybox), Nix, cryptsetup, dm-crypt kernel module, SEV-SNP attestation.

**Design doc:** `docs/plans/2026-03-05-encrypted-rootfs-design.md`

---

### Task 1: Add dm-crypt kernel module and cryptsetup binary to initrd

**Files:**
- Modify: `nix/initrd.nix`

**Step 1: Add dm-crypt.ko to the dmModules derivation**

In `nix/initrd.nix`, the `dmModules` derivation decompresses dm-verity modules. Add `dm-crypt.ko`:

```nix
dmModules = pkgs.runCommand "dm-verity-modules" {
  nativeBuildInputs = [ pkgs.xz ];
} ''
  mkdir -p $out
  xz -d -k -c ${modDir}/drivers/dax/dax.ko.xz > $out/dax.ko
  xz -d -k -c ${modDir}/drivers/md/dm-mod.ko.xz > $out/dm-mod.ko
  xz -d -k -c ${modDir}/drivers/md/dm-bufio.ko.xz > $out/dm-bufio.ko
  xz -d -k -c ${modDir}/drivers/md/dm-verity.ko.xz > $out/dm-verity.ko
  xz -d -k -c ${modDir}/drivers/md/dm-crypt.ko.xz > $out/dm-crypt.ko
'';
```

**Step 2: Add dm-crypt.ko and cryptsetup to initrd contents**

Add two new entries to the `contents` list:

```nix
{ object = "${staticCryptsetup}/bin/cryptsetup"; symlink = "/bin/cryptsetup"; }
{ object = "${dmModules}/dm-crypt.ko"; symlink = "/lib/modules/dm-crypt.ko"; }
```

`staticCryptsetup` is already defined (`pkgs.pkgsStatic.cryptsetup`) — the same package that provides `veritysetup`. No new Nix dependency needed.

**Step 3: Verify the build**

Run: `nix build .#initrd --no-link --print-out-paths` from `nix/`

Expected: Build succeeds. Verify the initrd contains the new files:
```bash
result=$(nix build .#initrd --no-link --print-out-paths)
# Check that dm-crypt.ko and cryptsetup are included
file "$result/initrd"  # Should be a cpio archive
```

**Step 4: Commit**

```bash
git add nix/initrd.nix
git commit -m "feat: add dm-crypt module and cryptsetup binary to initrd"
```

---

### Task 2: Add secret injection endpoint to attest-agent

**Files:**
- Create: `crates/aleph-attest-agent/src/secrets.rs`
- Modify: `crates/aleph-attest-agent/src/main.rs`
- Modify: `crates/aleph-attest-agent/src/proxy.rs`

**Step 1: Create the secrets module**

Create `crates/aleph-attest-agent/src/secrets.rs`:

```rust
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};
use tracing::info;

/// Directory where injected secrets are written as individual files.
const SECRETS_DIR: &str = "/tmp/secrets";

/// Atomic flag to enforce one-shot injection.
static SECRETS_INJECTED: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize)]
pub struct InjectSecretRequest {
    #[serde(flatten)]
    pub secrets: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct InjectSecretResponse {
    pub injected: Vec<String>,
}

/// POST /confidential/inject-secret
///
/// Accepts a JSON object of key-value pairs. Each key is written as a file
/// under /tmp/secrets/<key> containing the value. One-shot: returns 409 on
/// subsequent calls.
pub async fn inject_secret_handler(
    body: web::Json<InjectSecretRequest>,
) -> HttpResponse {
    // Enforce one-shot semantics.
    if SECRETS_INJECTED.swap(true, Ordering::SeqCst) {
        return HttpResponse::Conflict()
            .json(serde_json::json!({"error": "secrets already injected"}));
    }

    if body.secrets.is_empty() {
        SECRETS_INJECTED.store(false, Ordering::SeqCst);
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "no secrets provided"}));
    }

    // Create secrets directory.
    let secrets_dir = Path::new(SECRETS_DIR);
    if let Err(e) = std::fs::create_dir_all(secrets_dir) {
        SECRETS_INJECTED.store(false, Ordering::SeqCst);
        tracing::error!("failed to create secrets directory: {e}");
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": "failed to create secrets directory"}));
    }

    // Write each secret as a file.
    let mut injected = Vec::new();
    for (key, value) in &body.secrets {
        // Reject path traversal attempts.
        if key.contains('/') || key.contains("..") || key.is_empty() {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid secret key: {key}")}));
        }

        let path = secrets_dir.join(key);
        if let Err(e) = std::fs::write(&path, value) {
            tracing::error!("failed to write secret {key}: {e}");
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": format!("failed to write secret: {key}")}));
        }
        info!(key = %key, "injected secret");
        injected.push(key.clone());
    }

    HttpResponse::Ok().json(InjectSecretResponse { injected })
}
```

**Step 2: Register the endpoint in main.rs**

In `crates/aleph-attest-agent/src/main.rs`, add `mod secrets;` at the top and register the route:

```rust
mod secrets;
// ...
use secrets::inject_secret_handler;

// In HttpServer::new closure, add before default_service:
.route(
    "/confidential/inject-secret",
    web::post().to(inject_secret_handler),
)
```

The full route registration block becomes:

```rust
HttpServer::new(move || {
    App::new()
        .app_data(app_state.clone())
        .route(
            "/.well-known/attestation",
            web::get().to(attestation_endpoint),
        )
        .route(
            "/confidential/inject-secret",
            web::post().to(inject_secret_handler),
        )
        .default_service(web::to(proxy_handler))
})
```

**Step 3: Verify it compiles**

Run: `cargo check -p aleph-attest-agent` from the workspace root.

Expected: Compiles without errors.

**Step 4: Commit**

```bash
git add crates/aleph-attest-agent/src/secrets.rs
git add crates/aleph-attest-agent/src/main.rs
git commit -m "feat: add POST /confidential/inject-secret endpoint to attest-agent"
```

---

### Task 3: Add LUKS boot mode to init.sh

**Files:**
- Modify: `nix/init.sh`

The init script needs to:
1. Parse `luks=1` from kernel cmdline
2. In LUKS mode: start attest-agent early, wait for key, unlock, mount
3. In non-LUKS mode: keep existing behavior (dm-verity or direct mount, attest-agent starts after)

**Step 1: Add LUKS cmdline parsing**

After the `roothash` parsing (line 45), add:

```sh
# Parse luks flag from kernel command line (if present).
luks=$(/bin/busybox sed -n 's/.*luks=\([^ ]*\).*/\1/p' /proc/cmdline)
```

**Step 2: Add LUKS boot path**

Replace the section from "Mount rootfs and start user application" (line 73) through "Start the attestation agent" (line 122) with a conditional flow. The new structure:

```sh
if [ "$luks" = "1" ]; then
    #
    # LUKS encrypted rootfs mode.
    # Start attest-agent first, wait for user to inject decryption key.
    #
    echo "init: LUKS mode — loading dm-crypt, starting attest-agent"

    # Load dm-crypt (dm-mod is a dependency, load it too).
    /bin/busybox insmod /lib/modules/dm-mod.ko 2>&1 || echo "init: warning: insmod dm-mod.ko failed"
    /bin/busybox insmod /lib/modules/dm-crypt.ko 2>&1 || echo "init: warning: insmod dm-crypt.ko failed"
    /bin/busybox mkdir -p /dev/mapper
    /bin/busybox mknod /dev/mapper/control c 10 236 2>/dev/null

    # Start attest-agent BEFORE rootfs mount (so user can send LUKS key).
    /bin/aleph-attest-agent --port 8443 &

    # Wait for LUKS passphrase (injected by user via attest-agent).
    /bin/busybox mkdir -p /tmp/secrets
    echo "init: waiting for LUKS passphrase at /tmp/secrets/luks_passphrase ..."
    n=0
    while [ ! -f /tmp/secrets/luks_passphrase ]; do
        /bin/busybox sleep 0.5
        n=$((n + 1))
        if [ "$n" -ge 600 ]; then
            echo "init: FATAL: LUKS passphrase not received within 300s"
            break
        fi
    done

    if [ -n "$blkdev" ] && [ -f /tmp/secrets/luks_passphrase ]; then
        echo "init: unlocking LUKS on ${blkdev}"
        /bin/cryptsetup luksOpen "$blkdev" cryptroot < /tmp/secrets/luks_passphrase 2>&1
        /bin/busybox rm -f /tmp/secrets/luks_passphrase

        /bin/busybox mkdir -p /mnt/root
        if /bin/busybox mount /dev/mapper/cryptroot /mnt/root 2>&1; then
            echo "init: mounted /dev/mapper/cryptroot at /mnt/root"
            if [ -x /mnt/root/sbin/init ]; then
                echo "init: starting /sbin/init from rootfs"
                /bin/busybox chroot /mnt/root /sbin/init &
            else
                echo "init: WARNING: no /sbin/init found in rootfs"
            fi
        else
            echo "init: FATAL: failed to mount /dev/mapper/cryptroot"
        fi
    else
        echo "init: FATAL: no block device or no passphrase — cannot mount rootfs"
    fi
else
    #
    # dm-verity or plain rootfs mode (existing behavior).
    #

    # Load dm-verity kernel modules if verity is requested.
    if [ -n "$roothash" ]; then
        echo "init: loading dm-verity kernel modules"
        /bin/busybox insmod /lib/modules/dax.ko 2>&1 || echo "init: warning: insmod dax.ko failed"
        /bin/busybox insmod /lib/modules/dm-mod.ko 2>&1 || echo "init: warning: insmod dm-mod.ko failed"
        /bin/busybox insmod /lib/modules/dm-bufio.ko 2>&1 || echo "init: warning: insmod dm-bufio.ko failed"
        /bin/busybox insmod /lib/modules/dm-verity.ko 2>&1 || echo "init: warning: insmod dm-verity.ko failed"
        /bin/busybox mkdir -p /dev/mapper
        /bin/busybox mknod /dev/mapper/control c 10 236
    fi

    # Wait for block device to appear.
    # (blkdev already detected above)

    # Mount rootfs and start user application.
    if [ -n "$blkdev" ]; then
        /bin/busybox mkdir -p /mnt/root

        if [ -n "$roothash" ]; then
            # dm-verity path (unchanged)
            hashdev=""
            n=0
            while [ "$n" -lt 30 ]; do
                if [ -b /dev/vdb ]; then
                    hashdev="/dev/vdb"
                    break
                fi
                /bin/busybox sleep 0.1
                n=$((n + 1))
            done

            if [ -z "$hashdev" ]; then
                echo "init: FATAL: roothash set but /dev/vdb (hash tree) not found"
            else
                echo "init: setting up dm-verity on ${blkdev} with hash tree ${hashdev}"
                echo "init: roothash=${roothash}"
                if /bin/veritysetup open "$blkdev" verity-root "$hashdev" "$roothash" 2>&1; then
                    echo "init: mounting /dev/mapper/verity-root"
                    /bin/busybox mount -t ext4 -o ro /dev/mapper/verity-root /mnt/root || echo "init: verity mount failed"
                else
                    echo "init: FATAL: dm-verity verification failed — rootfs may be tampered"
                fi
            fi
        else
            # No dm-verity: direct mount
            echo "init: mounting ${blkdev} (no dm-verity)"
            if ! /bin/busybox mount -o ro "$blkdev" /mnt/root; then
                echo "init: mount failed, trying without readonly"
                /bin/busybox mount "$blkdev" /mnt/root || echo "init: mount failed completely"
            fi
        fi

        if [ -x /mnt/root/sbin/init ]; then
            echo "init: starting /sbin/init from rootfs"
            /bin/busybox chroot /mnt/root /sbin/init &
        else
            echo "init: WARNING: no /sbin/init found in rootfs"
        fi
    else
        echo "init: no block device found, skipping rootfs mount"
    fi

    # Start the attestation agent (after rootfs mount in non-LUKS mode).
    /bin/aleph-attest-agent --port 8443 --upstream http://127.0.0.1:8080 &
fi

# Wait for children.
wait
```

Note: In LUKS mode, the attest-agent is started without `--upstream` since the user app isn't running yet when attest-agent starts. Once the rootfs is unlocked and `/sbin/init` starts the app, the upstream becomes available. The default `--upstream http://127.0.0.1:8080` still works — requests will just get 502 until the app is ready.

**Step 2: Verify syntax**

Run: `bash -n nix/init.sh`

Expected: No syntax errors. (Note: this only checks bash syntax; busybox sh is compatible for this script.)

**Step 3: Commit**

```bash
git add nix/init.sh
git commit -m "feat: add LUKS encrypted rootfs boot mode to init.sh"
```

---

### Task 4: Add `luks=1` to kernel cmdline in compute-node

**Files:**
- Modify: `proto/compute.proto`
- Modify: `crates/aleph-compute-node/src/verity.rs`
- Modify: `crates/aleph-compute-node/src/vm/manager.rs`

**Step 1: Add `encrypted` field to CreateVmRequest**

In `proto/compute.proto`, add to `CreateVmRequest`:

```protobuf
message CreateVmRequest {
  string vm_id = 1;
  string kernel = 2;
  string initrd = 3;
  repeated DiskConfig disks = 4;
  uint32 vcpus = 5;
  uint32 memory_mb = 6;
  TeeConfig tee = 7;
  string ipv6_address = 8;
  uint32 ipv6_prefix_len = 9;
  bool encrypted = 10;          // true = LUKS rootfs, skip dm-verity
}
```

**Step 2: Update `build_kernel_cmdline` to handle LUKS mode**

In `crates/aleph-compute-node/src/verity.rs`, modify the function:

```rust
/// Build the kernel command line based on rootfs mode.
///
/// - `roothash` set: dm-verity mode (integrity-verified rootfs)
/// - `encrypted` true: LUKS mode (encrypted rootfs, key via attest-agent)
/// - neither: plain direct mount
pub fn build_kernel_cmdline(roothash: Option<&str>, encrypted: bool) -> String {
    if encrypted {
        "console=ttyS0 luks=1".to_string()
    } else {
        match roothash {
            Some(hash) => format!("console=ttyS0 root=/dev/mapper/verity-root ro roothash={hash}"),
            None => "console=ttyS0 root=/dev/vda ro".to_string(),
        }
    }
}
```

**Step 3: Update tests**

In the same file, update and add tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_kernel_cmdline_no_verity() {
        let cmdline = build_kernel_cmdline(None, false);
        assert_eq!(cmdline, "console=ttyS0 root=/dev/vda ro");
    }

    #[test]
    fn test_build_kernel_cmdline_with_verity() {
        let hash = "abc123def456";
        let cmdline = build_kernel_cmdline(Some(hash), false);
        assert!(cmdline.contains("roothash=abc123def456"));
        assert!(cmdline.contains("verity-root"));
    }

    #[test]
    fn test_build_kernel_cmdline_luks() {
        let cmdline = build_kernel_cmdline(None, true);
        assert_eq!(cmdline, "console=ttyS0 luks=1");
        assert!(!cmdline.contains("roothash"));
        assert!(!cmdline.contains("verity-root"));
    }

    #[test]
    fn test_build_kernel_cmdline_luks_ignores_roothash() {
        // If both are set, LUKS takes precedence (they're mutually exclusive).
        let cmdline = build_kernel_cmdline(Some("abc123"), true);
        assert_eq!(cmdline, "console=ttyS0 luks=1");
    }
}
```

**Step 4: Update call sites in manager.rs**

In `crates/aleph-compute-node/src/vm/manager.rs`, the `create_vm` method calls `build_kernel_cmdline`. Update the call to pass the `encrypted` field from the request.

Find where `build_kernel_cmdline` is called (in the `create_vm` method). The logic currently is:

```rust
let kernel_cmdline = if let Some(rootfs_disk) = config.disks.first() {
    match verity::ensure_verity(&rootfs_disk.path) {
        Ok(vinfo) => { /* ... */ verity::build_kernel_cmdline(Some(&vinfo.root_hash)) }
        Err(e) => { /* ... */ verity::build_kernel_cmdline(None) }
    }
} else {
    verity::build_kernel_cmdline(None)
};
```

Change to:

```rust
let encrypted = config.encrypted;
let kernel_cmdline = if encrypted {
    // LUKS mode: skip dm-verity, user will inject key via attest-agent.
    verity::build_kernel_cmdline(None, true)
} else if let Some(rootfs_disk) = config.disks.first() {
    match verity::ensure_verity(&rootfs_disk.path) {
        Ok(vinfo) => {
            config.disks.insert(1, DiskConfig {
                path: vinfo.hashtree_path,
                readonly: true,
                format: "raw".to_string(),
            });
            verity::build_kernel_cmdline(Some(&vinfo.root_hash), false)
        }
        Err(e) => {
            warn!(vm_id = %vm_id, error = %e, "dm-verity setup failed, falling back to direct mount");
            verity::build_kernel_cmdline(None, false)
        }
    }
} else {
    verity::build_kernel_cmdline(None, false)
};
```

Note: The `encrypted` field comes from the protobuf `CreateVmRequest.encrypted`. After `cargo build`, the generated Rust code will include this field as `bool` on the request struct.

**Step 5: Run tests**

Run: `cargo test -p aleph-compute-node`

Expected: All tests pass, including the new LUKS cmdline tests.

**Step 6: Commit**

```bash
git add proto/compute.proto
git add crates/aleph-compute-node/src/verity.rs
git add crates/aleph-compute-node/src/vm/manager.rs
git commit -m "feat: add LUKS boot mode to kernel cmdline builder and proto"
```

---

### Task 5: Add `inject-secret` command to attest-cli

**Files:**
- Modify: `crates/aleph-attest-cli/src/main.rs`
- Modify: `crates/aleph-attest-cli/src/client.rs`

**Step 1: Refactor CLI to use subcommands**

Replace the boolean flag approach with clap subcommands in `crates/aleph-attest-cli/src/main.rs`:

```rust
mod client;
mod verify;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "aleph-attest-cli", about = "Client-side TEE attestation verifier")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Layer 2: Make an attested HTTP request (TLS-bound verification)
    Attest {
        /// URL to connect to
        #[arg(long)]
        url: String,
        /// AMD product name for certificate chain validation
        #[arg(long, default_value = "Genoa")]
        amd_product: String,
        /// Expected VM measurement as hex string
        #[arg(long)]
        expected_measurement: Option<String>,
    },
    /// Layer 3: Request fresh attestation with a random nonce
    FreshAttest {
        /// URL to connect to
        #[arg(long)]
        url: String,
        /// AMD product name for certificate chain validation
        #[arg(long, default_value = "Genoa")]
        amd_product: String,
        /// Expected VM measurement as hex string
        #[arg(long)]
        expected_measurement: Option<String>,
    },
    /// Inject secrets into a running CVM via the attested TLS channel
    InjectSecret {
        /// URL to connect to (e.g., https://[vm-ipv6]:8443)
        #[arg(long)]
        url: String,
        /// AMD product name for certificate chain validation
        #[arg(long, default_value = "Genoa")]
        amd_product: String,
        /// Expected VM measurement as hex string
        #[arg(long)]
        expected_measurement: Option<String>,
        /// Secrets as key=value pairs (e.g., luks_passphrase=mysecret)
        #[arg(long = "secret", value_parser = parse_key_value)]
        secrets: Vec<(String, String)>,
    },
}

fn parse_key_value(s: &str) -> Result<(String, String), String> {
    let pos = s.find('=').ok_or_else(|| format!("invalid key=value: no '=' in '{s}'"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Attest { url, amd_product, expected_measurement } => {
            let expected = expected_measurement
                .as_deref()
                .map(|h| hex::decode(h).context("invalid hex in --expected-measurement"))
                .transpose()?;

            println!("Making attested request to {url}...");
            let response = client::attested_request(&url, &amd_product, expected.as_deref()).await?;

            println!("Attestation valid: {}", response.attestation_valid);
            println!("Summary:           {}", response.attestation_summary);
            println!("Measurement:       {}", hex::encode(&response.measurement));
            println!("HTTP status:       {}", response.status);
            println!("Response body:");
            println!("{}", response.body);
        }
        Commands::FreshAttest { url, amd_product, expected_measurement } => {
            let expected = expected_measurement
                .as_deref()
                .map(|h| hex::decode(h).context("invalid hex in --expected-measurement"))
                .transpose()?;

            println!("Requesting fresh attestation from {url}...");
            let report = client::fresh_attestation(&url, &amd_product, expected.as_deref()).await?;

            println!("Fresh attestation verified successfully!");
            println!("  TEE type:     {:?}", report.tee_type);
            println!("  Measurement:  {}", hex::encode(&report.measurement));
            println!("  Report data:  {}", hex::encode(report.report_data));
        }
        Commands::InjectSecret { url, amd_product, expected_measurement, secrets } => {
            let expected = expected_measurement
                .as_deref()
                .map(|h| hex::decode(h).context("invalid hex in --expected-measurement"))
                .transpose()?;

            if secrets.is_empty() {
                anyhow::bail!("at least one --secret key=value is required");
            }

            println!("Injecting {} secret(s) into {url}...", secrets.len());
            let result = client::inject_secret(&url, &amd_product, expected.as_deref(), &secrets).await?;

            println!("Attestation verified, secrets injected:");
            for key in &result.injected {
                println!("  - {key}");
            }
        }
    }

    Ok(())
}
```

**Step 2: Add `inject_secret` function to client.rs**

Add to `crates/aleph-attest-cli/src/client.rs`:

```rust
use std::collections::HashMap;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct InjectSecretResponse {
    pub injected: Vec<String>,
}

/// Inject secrets into a CVM via the attested TLS channel.
///
/// 1. TLS handshake verifies attestation (key binding + optional measurement)
/// 2. Full AMD certificate chain verified
/// 3. Secrets POSTed to /confidential/inject-secret
pub async fn inject_secret(
    base_url: &str,
    product: &str,
    expected_measurement: Option<&[u8]>,
    secrets: &[(String, String)],
) -> Result<InjectSecretResponse> {
    let base = url::Url::parse(base_url).context("failed to parse base URL")?;
    let inject_url = base
        .join("confidential/inject-secret")
        .context("failed to construct inject-secret URL")?;

    let verifier = SnpCertVerifier::new(expected_measurement.map(|m| m.to_vec()));
    let client = build_attested_client(&verifier)?;

    // Build the secrets map.
    let secrets_map: HashMap<&str, &str> = secrets
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let response = client
        .post(inject_url.as_str())
        .json(&secrets_map)
        .send()
        .await
        .context("failed to send inject-secret request")?;

    // Verify attestation after TLS handshake.
    let report = verifier
        .get_report()
        .context("no attestation report extracted from TLS handshake")?;
    let result = verify_sev_snp_report(&report, product)
        .await
        .context("SEV-SNP report verification failed")?;
    if !result.valid {
        bail!("attestation invalid: {}", result.summary);
    }

    let status = response.status().as_u16();
    if status == 409 {
        bail!("secrets already injected (409 Conflict)");
    }
    if status != 200 {
        let body = response.text().await.unwrap_or_default();
        bail!("inject-secret failed with status {status}: {body}");
    }

    let resp: InjectSecretResponse = response
        .json()
        .await
        .context("failed to parse inject-secret response")?;

    Ok(resp)
}
```

**Step 3: Add `url` dependency if missing**

Check `crates/aleph-attest-cli/Cargo.toml` — `url` is already used in `fresh_attestation`. If not in Cargo.toml, add it.

**Step 4: Verify it compiles**

Run: `cargo check -p aleph-attest-cli`

Expected: Compiles without errors.

**Step 5: Commit**

```bash
git add crates/aleph-attest-cli/src/main.rs
git add crates/aleph-attest-cli/src/client.rs
git commit -m "feat: add inject-secret command to attest-cli with subcommand refactor"
```

---

### Task 6: Wire up encrypted flag in scheduler-agent

**Files:**
- Modify: `crates/aleph-scheduler-agent/src/adapter.rs`

The scheduler-agent translates Aleph messages into `CreateVmRequest`. When the message has `trusted_execution` with an encrypted rootfs, set `encrypted = true`.

**Step 1: Check how the adapter currently constructs CreateVmRequest**

Read `crates/aleph-scheduler-agent/src/adapter.rs` to find where `CreateVmRequest` is built.

**Step 2: Add encrypted field**

In the adapter's `CreateVmRequest` construction, add:

```rust
encrypted: msg.environment.trusted_execution
    .as_ref()
    .map(|te| te.encrypted)
    .unwrap_or(false),
```

The exact field name in the Aleph message may differ — the adapter needs to detect that the rootfs is LUKS-encrypted. This could be:
- A new `encrypted: bool` field in the `trusted_execution` config
- Or inferred from the presence of `trusted_execution` itself (if all confidential VMs use encrypted disks)

For now, add an explicit `encrypted` boolean to the trusted_execution config in the message format, and pass it through.

**Step 3: Verify it compiles**

Run: `cargo check -p aleph-scheduler-agent`

**Step 4: Commit**

```bash
git add crates/aleph-scheduler-agent/src/adapter.rs
git commit -m "feat: pass encrypted flag through scheduler-agent to compute-node"
```

---

### Task 7: End-to-end test with a LUKS rootfs image

**Files:**
- Modify: `scripts/demo.sh` (or create a new `scripts/demo-luks.sh`)

**Step 1: Create a LUKS-encrypted demo rootfs**

Build a test LUKS image using the existing fib-service rootfs:

```bash
# Build the plain rootfs first.
nix build .#rootfs --no-link --print-out-paths
PLAIN_ROOTFS=$(nix build .#rootfs --no-link --print-out-paths)

# Create a LUKS container and copy the rootfs into it.
LUKS_IMG=$(mktemp /tmp/rootfs-luks.XXXXXX.img)
PASSPHRASE="test-passphrase-123"

# Create a file slightly larger than the plain rootfs.
SIZE=$(stat -c %s "$PLAIN_ROOTFS")
LUKS_SIZE=$((SIZE + 16*1024*1024))  # add 16MB for LUKS header
truncate -s "$LUKS_SIZE" "$LUKS_IMG"

# Format as LUKS and open.
echo -n "$PASSPHRASE" | cryptsetup luksFormat --batch-mode "$LUKS_IMG" -
echo -n "$PASSPHRASE" | cryptsetup luksOpen "$LUKS_IMG" test-cryptroot -

# Copy plain rootfs contents into the LUKS container.
dd if="$PLAIN_ROOTFS" of=/dev/mapper/test-cryptroot bs=4M
cryptsetup luksClose test-cryptroot

echo "LUKS image: $LUKS_IMG"
echo "Passphrase: $PASSPHRASE"
```

**Step 2: Launch VM with luks=1**

Use the existing demo flow but with:
- The LUKS image as the rootfs disk
- `encrypted: true` in the CreateVm request (which sets `luks=1` in cmdline)
- No verity hash tree disk

**Step 3: Inject the secret**

After the VM boots and attest-agent is listening:

```bash
aleph-attest-cli inject-secret \
    --url https://[vm-ipv6]:8443 \
    --secret luks_passphrase=test-passphrase-123
```

Expected: The CLI verifies attestation, injects the secret, and the VM unlocks the rootfs and starts the app.

**Step 4: Verify the app is running**

```bash
aleph-attest-cli attest --url https://[vm-ipv6]:8443
```

Expected: The fib-service responds through the attested proxy.

**Step 5: Commit**

```bash
git add scripts/demo-luks.sh
git commit -m "feat: add end-to-end LUKS rootfs demo script"
```
