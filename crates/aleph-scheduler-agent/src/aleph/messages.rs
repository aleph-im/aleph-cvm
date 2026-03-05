//! Aleph message types for executable content.
//!
//! These are local representations of the `aleph-message` Python models.
//! Only the fields relevant to VM orchestration are included.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Aleph item hash (content-addressed identifier).
pub type ItemHash = String;

/// Top-level discriminator for VM type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineType {
    /// Short-lived function execution.
    VmFunction,
    /// Long-running instance.
    VmInstance,
}

/// Resources allocated to a VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineResources {
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    /// Memory in MiB.
    #[serde(default = "default_memory")]
    pub memory: u32,
    /// Execution time limit in seconds (functions only).
    #[serde(default = "default_seconds")]
    pub seconds: u32,
    /// Port forwarding configuration.
    #[serde(default)]
    pub published_ports: Vec<PublishedPort>,
}

fn default_vcpus() -> u32 {
    1
}
fn default_memory() -> u32 {
    128
}
fn default_seconds() -> u32 {
    1
}

/// Port to publish from the VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedPort {
    pub protocol: String,
    pub port: u16,
}

// ── Volume types ────────────────────────────────────────────────────────────

/// Discriminated volume union.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MachineVolume {
    Immutable(ImmutableVolume),
    Persistent(PersistentVolume),
    Ephemeral(EphemeralVolume),
}

impl MachineVolume {
    pub fn mount(&self) -> &str {
        match self {
            MachineVolume::Immutable(v) => &v.mount,
            MachineVolume::Persistent(v) => &v.mount,
            MachineVolume::Ephemeral(v) => &v.mount,
        }
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, MachineVolume::Immutable(_))
    }
}

/// Read-only volume referenced by hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImmutableVolume {
    #[serde(rename = "ref")]
    pub item_ref: ItemHash,
    pub mount: String,
    #[serde(default = "default_true")]
    pub use_latest: bool,
}

/// Persistent volume with optional parent image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentVolume {
    pub mount: String,
    pub name: Option<String>,
    pub size_mib: u64,
    pub parent: Option<ParentVolume>,
    #[serde(default)]
    pub persistence: VolumePersistence,
}

/// Ephemeral volume (created fresh each boot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EphemeralVolume {
    pub mount: String,
    pub ephemeral: bool,
    pub size_mib: u64,
}

/// Reference to a parent volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParentVolume {
    #[serde(rename = "ref")]
    pub item_ref: ItemHash,
    #[serde(default = "default_true")]
    pub use_latest: bool,
}

/// Volume persistence mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumePersistence {
    #[default]
    Host,
    Store,
}

fn default_true() -> bool {
    true
}

// ── Root filesystem (instances only) ────────────────────────────────────────

/// Root filesystem for instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootfsVolume {
    pub parent: ParentVolume,
    #[serde(default)]
    pub persistence: VolumePersistence,
    pub size_mib: u64,
}

// ── Environment / TEE ───────────────────────────────────────────────────────

/// Instance environment configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceEnvironment {
    #[serde(default)]
    pub internet: bool,
    #[serde(default)]
    pub aleph_api: bool,
    pub hypervisor: Option<HypervisorType>,
    pub trusted_execution: Option<TrustedExecutionEnvironment>,
}

/// Function environment configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionEnvironment {
    #[serde(default)]
    pub reproducible: bool,
    #[serde(default)]
    pub internet: bool,
    #[serde(default)]
    pub aleph_api: bool,
}

/// Hypervisor type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HypervisorType {
    Qemu,
    Firecracker,
}

/// TEE configuration from Aleph messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedExecutionEnvironment {
    /// Custom OVMF firmware hash.
    pub firmware: Option<ItemHash>,
    /// SEV policy flags.
    #[serde(default)]
    pub policy: u64,
    /// Whether the rootfs is LUKS-encrypted (user injects key via attest-agent).
    #[serde(default)]
    pub encrypted: bool,
}

// ── Code (programs only) ────────────────────────────────────────────────────

/// Encoding of code/data content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Plain,
    Zip,
    Squashfs,
}

/// Program code reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeContent {
    pub encoding: Encoding,
    pub entrypoint: String,
    #[serde(rename = "ref")]
    pub item_ref: ItemHash,
    pub interface: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

/// Runtime reference (e.g. Python squashfs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionRuntime {
    #[serde(rename = "ref")]
    pub item_ref: ItemHash,
    #[serde(default = "default_true")]
    pub use_latest: bool,
    #[serde(default)]
    pub comment: String,
}

// ── Payment ─────────────────────────────────────────────────────────────────

/// Payment type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentType {
    Hold,
    Superfluid,
    Credit,
}

/// Payment configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payment {
    pub chain: Option<String>,
    pub receiver: Option<String>,
    #[serde(rename = "type")]
    pub payment_type: PaymentType,
}

impl Payment {
    pub fn is_stream(&self) -> bool {
        self.payment_type == PaymentType::Superfluid
    }
}

// ── Executable content (the top-level message) ──────────────────────────────

/// Aleph executable message — the union of program and instance content.
///
/// This is the parsed form of an Aleph PROGRAM or INSTANCE message.
/// The scheduler agent receives these and translates them into
/// `CreateVmRequest` proto messages for the compute node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableMessage {
    /// Content hash (the Aleph item_hash of this message).
    pub item_hash: ItemHash,
    /// Machine type discriminator.
    #[serde(rename = "type")]
    pub machine_type: MachineType,
    /// Resource requirements.
    pub resources: MachineResources,
    /// Additional volumes.
    #[serde(default)]
    pub volumes: Vec<MachineVolume>,
    /// Environment variables for the VM.
    #[serde(default)]
    pub variables: HashMap<String, String>,
    /// Whether this message can be amended.
    #[serde(default)]
    pub allow_amend: bool,
    /// Hash of message this replaces (amendment chain).
    pub replaces: Option<ItemHash>,
    /// Payment configuration.
    pub payment: Option<Payment>,

    // ── Instance-specific fields ──
    /// Instance environment (instances only).
    pub environment: Option<InstanceEnvironment>,
    /// Root filesystem (instances only).
    pub rootfs: Option<RootfsVolume>,

    // ── Program-specific fields ──
    /// Program code (programs only).
    pub code: Option<CodeContent>,
    /// Runtime (programs only).
    pub runtime: Option<FunctionRuntime>,
}

impl ExecutableMessage {
    /// Whether this message describes a confidential VM.
    pub fn is_confidential(&self) -> bool {
        self.environment
            .as_ref()
            .and_then(|e| e.trusted_execution.as_ref())
            .is_some()
    }

    /// Whether this is a persistent/long-running execution.
    pub fn is_persistent(&self) -> bool {
        self.machine_type == MachineType::VmInstance
    }

    /// SEV policy if this is a confidential VM.
    pub fn sev_policy(&self) -> Option<u64> {
        self.environment
            .as_ref()?
            .trusted_execution
            .as_ref()
            .map(|t| t.policy)
    }

    /// Whether the rootfs is LUKS-encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.environment
            .as_ref()
            .and_then(|e| e.trusted_execution.as_ref())
            .map(|t| t.encrypted)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_instance_message() {
        let json = r#"{
            "item_hash": "abc123def456",
            "type": "vm_instance",
            "resources": {
                "vcpus": 4,
                "memory": 2048,
                "published_ports": [
                    {"protocol": "tcp", "port": 8080}
                ]
            },
            "volumes": [
                {"ref": "vol_hash_1", "mount": "/data", "use_latest": true}
            ],
            "variables": {"ENV": "production"},
            "allow_amend": true,
            "environment": {
                "internet": true,
                "hypervisor": "qemu",
                "trusted_execution": {
                    "policy": 196608
                }
            },
            "rootfs": {
                "parent": {"ref": "rootfs_hash", "use_latest": true},
                "persistence": "host",
                "size_mib": 10240
            }
        }"#;

        let msg: ExecutableMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.machine_type, MachineType::VmInstance);
        assert_eq!(msg.resources.vcpus, 4);
        assert_eq!(msg.resources.memory, 2048);
        assert!(msg.is_confidential());
        assert!(msg.is_persistent());
        assert_eq!(msg.sev_policy(), Some(196608));
        assert_eq!(msg.rootfs.as_ref().unwrap().parent.item_ref, "rootfs_hash");
        assert_eq!(msg.volumes.len(), 1);
        assert_eq!(msg.resources.published_ports.len(), 1);
    }

    #[test]
    fn test_deserialize_program_message() {
        let json = r#"{
            "item_hash": "prog_hash_789",
            "type": "vm_function",
            "resources": {"vcpus": 1, "memory": 256, "seconds": 30},
            "code": {
                "encoding": "squashfs",
                "entrypoint": "main:app",
                "ref": "code_hash",
                "interface": "asgi"
            },
            "runtime": {
                "ref": "runtime_hash",
                "comment": "Python 3.11"
            }
        }"#;

        let msg: ExecutableMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.machine_type, MachineType::VmFunction);
        assert!(!msg.is_persistent());
        assert!(!msg.is_confidential());
        assert_eq!(msg.code.as_ref().unwrap().entrypoint, "main:app");
        assert_eq!(msg.runtime.as_ref().unwrap().item_ref, "runtime_hash");
    }

    #[test]
    fn test_machine_resources_defaults() {
        let json = r#"{}"#;
        let res: MachineResources = serde_json::from_str(json).unwrap();
        assert_eq!(res.vcpus, 1);
        assert_eq!(res.memory, 128);
        assert_eq!(res.seconds, 1);
        assert!(res.published_ports.is_empty());
    }

    #[test]
    fn test_volume_discriminated_union() {
        // Immutable volume
        let json = r#"{"ref": "abc", "mount": "/mnt/data", "use_latest": true}"#;
        let vol: MachineVolume = serde_json::from_str(json).unwrap();
        assert!(vol.is_read_only());
        assert_eq!(vol.mount(), "/mnt/data");

        // Ephemeral volume
        let json = r#"{"mount": "/tmp", "ephemeral": true, "size_mib": 500}"#;
        let vol: MachineVolume = serde_json::from_str(json).unwrap();
        assert!(!vol.is_read_only());

        // Persistent volume
        let json = r#"{"mount": "/data", "size_mib": 2048, "persistence": "host"}"#;
        let vol: MachineVolume = serde_json::from_str(json).unwrap();
        assert!(!vol.is_read_only());
    }
}
