//! Volume download from Aleph storage.
//!
//! Downloads content-addressed volumes (code, runtime, data) from an Aleph
//! connector node and caches them locally.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

use super::messages::ItemHash;

/// Content-addressed local cache for Aleph volumes.
pub struct VolumeCache {
    /// Base directory for cached volumes.
    base_dir: PathBuf,
    /// Aleph connector URL (e.g. "https://official.aleph.cloud").
    connector_url: String,
    /// HTTP client for downloads.
    client: reqwest::Client,
}

impl VolumeCache {
    pub fn new(base_dir: PathBuf, connector_url: String) -> Self {
        Self {
            base_dir,
            connector_url: connector_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Ensure a volume is cached locally, downloading if needed.
    ///
    /// Returns the local path to the cached file.
    pub async fn ensure_cached(
        &self,
        item_hash: &ItemHash,
        category: VolumeCategory,
    ) -> Result<PathBuf> {
        let cache_dir = self.base_dir.join(category.cache_subdir());
        tokio::fs::create_dir_all(&cache_dir)
            .await
            .context("creating cache directory")?;

        let local_path = cache_dir.join(item_hash);

        if local_path.exists() {
            debug!(hash = %item_hash, path = %local_path.display(), "volume already cached");
            return Ok(local_path);
        }

        let url = format!(
            "{}/download/{}/{}",
            self.connector_url,
            category.url_segment(),
            item_hash,
        );

        info!(hash = %item_hash, url = %url, "downloading volume");

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?
            .error_for_status()
            .with_context(|| format!("HTTP error for {url}"))?;

        // Stream to a temporary file, then rename (atomic on same fs)
        let tmp_path = local_path.with_extension("tmp");
        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .context("creating temp file")?;

        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("reading body from {url}"))?;

        file.write_all(&bytes).await.context("writing volume")?;
        file.flush().await?;
        drop(file);

        tokio::fs::rename(&tmp_path, &local_path)
            .await
            .context("renaming temp file to final path")?;

        info!(
            hash = %item_hash,
            path = %local_path.display(),
            size = bytes.len(),
            "volume cached"
        );

        Ok(local_path)
    }

    /// Verify a cached file against its expected hash.
    pub async fn verify_hash(path: &Path, expected_hash: &str) -> Result<bool> {
        let data = tokio::fs::read(path).await?;
        let digest = Sha256::digest(&data);
        let actual = hex::encode(digest);
        Ok(actual == expected_hash)
    }

    /// Remove a cached volume.
    pub async fn evict(&self, item_hash: &ItemHash, category: VolumeCategory) -> Result<()> {
        let path = self.base_dir.join(category.cache_subdir()).join(item_hash);
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
            debug!(hash = %item_hash, "evicted from cache");
        }
        Ok(())
    }
}

/// Category of downloadable volume (determines URL path and cache subdir).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeCategory {
    /// Program code archive.
    Code,
    /// Runtime image (e.g. Python squashfs).
    Runtime,
    /// Data volume.
    Data,
}

impl VolumeCategory {
    fn url_segment(self) -> &'static str {
        match self {
            VolumeCategory::Code => "code",
            VolumeCategory::Runtime => "runtime",
            VolumeCategory::Data => "data",
        }
    }

    fn cache_subdir(self) -> &'static str {
        match self {
            VolumeCategory::Code => "code",
            VolumeCategory::Runtime => "runtime",
            VolumeCategory::Data => "data",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_category_segments() {
        assert_eq!(VolumeCategory::Code.url_segment(), "code");
        assert_eq!(VolumeCategory::Runtime.url_segment(), "runtime");
        assert_eq!(VolumeCategory::Data.url_segment(), "data");
    }

    #[test]
    fn test_volume_category_cache_dirs() {
        assert_eq!(VolumeCategory::Code.cache_subdir(), "code");
        assert_eq!(VolumeCategory::Runtime.cache_subdir(), "runtime");
        assert_eq!(VolumeCategory::Data.cache_subdir(), "data");
    }

    #[tokio::test]
    async fn test_verify_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        tokio::fs::write(&path, b"hello world").await.unwrap();

        // SHA-256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(VolumeCache::verify_hash(&path, expected).await.unwrap());
        assert!(!VolumeCache::verify_hash(&path, "wrong").await.unwrap());
    }
}
