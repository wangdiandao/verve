//! Backup and synchronization subsystem for Verve (`0.3.0`).
//!
//! Operates exclusively on the syncable configuration domain managed by
//! `verve-config` through the [`BackupBackend`] extension trait.

use std::future::Future;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Metadata describing an immutable encrypted backup snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupMeta {
    /// Unique snapshot identifier assigned by the backend or exporter.
    pub id: String,
    /// Creation timestamp as Unix epoch seconds.
    pub created_at_unix_secs: u64,
    /// Encrypted archive payload size in bytes.
    pub size_bytes: u64,
    /// Hex-encoded SHA-256 digest of the encrypted archive payload.
    pub checksum_sha256: String,
}

/// Errors returned by [`BackupBackend`] implementations.
#[derive(Debug, Error)]
pub enum SyncError {
    /// Requested backup snapshot identifier was not found.
    #[error("backup snapshot not found: {0}")]
    NotFound(String),
    /// Backup backend operation failed.
    #[error("backup backend error: {0}")]
    Backend(String),
    /// Underlying I/O error during backup archive handling.
    #[error("sync I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Pluggable backup and synchronization backend (local encrypted archive, WebDAV, etc.).
pub trait BackupBackend: Send + Sync + 'static {
    /// Lists available backup snapshots exposed by the backend.
    fn list_snapshots(&self) -> impl Future<Output = Result<Vec<BackupMeta>, SyncError>> + Send;

    /// Uploads an encrypted backup archive and its associated metadata.
    fn push_snapshot(
        &self,
        archive: &[u8],
        meta: &BackupMeta,
    ) -> impl Future<Output = Result<(), SyncError>> + Send;

    /// Downloads the encrypted backup archive identified by `id`.
    fn pull_snapshot(&self, id: &str) -> impl Future<Output = Result<Vec<u8>, SyncError>> + Send;

    /// Deletes the backup snapshot identified by `id`.
    fn delete_snapshot(&self, id: &str) -> impl Future<Output = Result<(), SyncError>> + Send;
}
