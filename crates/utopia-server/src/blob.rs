//! BlobStore: the seam for storing and fetching the raw bytes of a file.
//!
//! Content-addressed -- the key is just the sha256 of the file content, and the interface has no
//! notion of a "path": locally it is a flat directory (pre-sharding), on object storage it is an
//! object key, and any KV store can implement it. Idempotence, dedup, and immutability (change
//! the content and the fingerprint changes, so an old version is never overwritten -- the
//! material basis for "version replay has something to show") all come free from "the content is
//! the address".
//!
//! At this stage the only implementation is local disk (data/files/{sha256}). Wiring up object
//! storage / network drives later (P5 connectors, shared storage for multi-instance deployments)
//! only takes a new implementation; the ingest/upload/parse/replay callers do not change a single
//! line. The config entry point UTOPIA_BLOB_BACKEND is reserved and currently accepts only
//! "local".

use std::path::PathBuf;

#[async_trait::async_trait]
pub trait BlobStore: Send + Sync {
    /// Idempotent write: skip if the same fingerprint already exists.
    async fn put(&self, sha256: &str, bytes: &[u8]) -> anyhow::Result<()>;
    async fn get(&self, sha256: &str) -> anyhow::Result<Vec<u8>>;
    #[allow(dead_code)] // interface completeness: future consumers on the replay/GC paths
    async fn exists(&self, sha256: &str) -> anyhow::Result<bool>;
}

/// The local-disk implementation: stored flat at `{dir}/{sha256}` (byte-for-byte identical to
/// the historical behaviour).
pub struct LocalBlobStore {
    dir: PathBuf,
}

impl LocalBlobStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

#[async_trait::async_trait]
impl BlobStore for LocalBlobStore {
    async fn put(&self, sha256: &str, bytes: &[u8]) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let path = self.dir.join(sha256);
        if !path.exists() {
            tokio::fs::write(&path, bytes).await?;
        }
        Ok(())
    }

    async fn get(&self, sha256: &str) -> anyhow::Result<Vec<u8>> {
        Ok(tokio::fs::read(self.dir.join(sha256)).await?)
    }

    async fn exists(&self, sha256: &str) -> anyhow::Result<bool> {
        Ok(self.dir.join(sha256).exists())
    }
}
