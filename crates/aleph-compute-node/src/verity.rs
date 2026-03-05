use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

/// Information about a rootfs's dm-verity hash tree.
#[derive(Debug, Clone)]
pub struct VerityInfo {
    /// The dm-verity root hash (hex string, lowercase).
    pub root_hash: String,
    /// Path to the hash tree file.
    pub hashtree_path: PathBuf,
}

/// Ensure dm-verity artifacts exist for the given rootfs image.
///
/// If `{rootfs_path}.verity` and `{rootfs_path}.roothash` exist and are
/// newer than the rootfs, returns the cached values. Otherwise, runs
/// `veritysetup format` to compute them.
pub fn ensure_verity(rootfs_path: &Path) -> Result<VerityInfo> {
    let hashtree_path = PathBuf::from(format!("{}.verity", rootfs_path.display()));
    let roothash_path = PathBuf::from(format!("{}.roothash", rootfs_path.display()));

    // Check if cached artifacts are still valid
    if hashtree_path.exists() && roothash_path.exists() {
        let rootfs_mtime = rootfs_path
            .metadata()
            .and_then(|m| m.modified())
            .ok();
        let cache_mtime = roothash_path
            .metadata()
            .and_then(|m| m.modified())
            .ok();

        if let (Some(rootfs_t), Some(cache_t)) = (rootfs_mtime, cache_mtime) {
            if cache_t >= rootfs_t {
                let root_hash = std::fs::read_to_string(&roothash_path)
                    .context("failed to read cached roothash")?
                    .trim()
                    .to_string();
                info!(rootfs = %rootfs_path.display(), root_hash = %root_hash, "using cached verity artifacts");
                return Ok(VerityInfo {
                    root_hash,
                    hashtree_path,
                });
            }
        }
    }

    // Compute verity hash tree
    info!(rootfs = %rootfs_path.display(), "computing dm-verity hash tree");
    let output = std::process::Command::new("veritysetup")
        .args([
            "format",
            &rootfs_path.display().to_string(),
            &hashtree_path.display().to_string(),
        ])
        .output()
        .context("failed to execute veritysetup format")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("veritysetup format failed: {stderr}");
    }

    // Parse root hash from stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    let root_hash = stdout
        .lines()
        .find(|line| line.starts_with("Root hash:"))
        .and_then(|line| line.split_whitespace().last())
        .map(|s| s.trim().to_lowercase())
        .context("failed to parse root hash from veritysetup output")?;

    // Cache the root hash
    std::fs::write(&roothash_path, &root_hash)
        .with_context(|| format!("failed to write {}", roothash_path.display()))?;

    info!(rootfs = %rootfs_path.display(), root_hash = %root_hash, "computed verity hash tree");

    Ok(VerityInfo {
        root_hash,
        hashtree_path,
    })
}

/// Build the kernel command line, optionally including a dm-verity root hash.
///
/// If `encrypted` is true, emits `luks=1` instead of any verity/root parameters
/// (the init script will prompt for a key via attest-agent).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_kernel_cmdline_no_verity() {
        let cmdline = build_kernel_cmdline(None, false);
        assert_eq!(cmdline, "console=ttyS0 root=/dev/vda ro");
        assert!(!cmdline.contains("roothash"));
        assert!(!cmdline.contains("verity-root"));
    }

    #[test]
    fn test_build_kernel_cmdline_with_verity() {
        let hash = "abc123def456";
        let cmdline = build_kernel_cmdline(Some(hash), false);
        assert_eq!(
            cmdline,
            "console=ttyS0 root=/dev/mapper/verity-root ro roothash=abc123def456"
        );
        assert!(cmdline.contains("verity-root"));
        assert!(cmdline.contains("roothash=abc123def456"));
        assert!(!cmdline.contains("/dev/vda"));
    }

    #[test]
    fn test_build_kernel_cmdline_no_ip() {
        let none = build_kernel_cmdline(None, false);
        let some = build_kernel_cmdline(Some("aabbccdd"), false);
        assert!(!none.contains("ip="));
        assert!(!some.contains("ip="));
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
        let cmdline = build_kernel_cmdline(Some("abc123"), true);
        assert_eq!(cmdline, "console=ttyS0 luks=1");
    }
}
