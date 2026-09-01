use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Proof of publication returned by a DA backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaReceipt {
    /// Keccak-256 hash of the exact bytes submitted to the DA layer.
    pub content_hash: [u8; 32],
    /// Batch sequence number this receipt corresponds to.
    pub sequence: u64,
}

/// Enriched DA record stored in [`CommitResult`]: receipt + which backend was used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaRecord {
    pub receipt: DaReceipt,
    pub backend: String,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over a data-availability backend.
///
/// Implementations must be `Send + Sync` so they can be stored in the async
/// committer task.  The method is synchronous — callers in async contexts that
/// need to avoid blocking the executor should wrap with `spawn_blocking`.
pub trait DataAvailabilityClient: Send + Sync {
    /// Publish `data` to the DA layer and return a receipt.
    fn submit(&self, sequence: u64, data: &[u8]) -> anyhow::Result<DaReceipt>;

    /// Human-readable name used in log messages.
    fn name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

// ---------------------------------------------------------------------------
// LocalDaClient — writes blobs to disk (dev / test)
// ---------------------------------------------------------------------------

pub struct LocalDaClient {
    dir: PathBuf,
}

impl LocalDaClient {
    pub fn new(dir: PathBuf) -> Self {
        LocalDaClient { dir }
    }
}

impl DataAvailabilityClient for LocalDaClient {
    fn submit(&self, sequence: u64, data: &[u8]) -> anyhow::Result<DaReceipt> {
        std::fs::create_dir_all(&self.dir)?;
        let hash = keccak256(data);
        let filename = format!("da_batch_{:016}.bin", sequence);
        std::fs::write(self.dir.join(&filename), data)?;
        Ok(DaReceipt {
            content_hash: hash,
            sequence,
        })
    }

    fn name(&self) -> &'static str {
        "local"
    }
}

// ---------------------------------------------------------------------------
// MockDaClient — in-memory, for tests
// ---------------------------------------------------------------------------

/// A record of a single call to [`MockDaClient::submit`].
#[derive(Debug, Clone)]
pub struct MockSubmission {
    pub sequence: u64,
    pub data: Vec<u8>,
    pub receipt: DaReceipt,
}

struct MockInner {
    submissions: Vec<MockSubmission>,
    fail: bool,
}

/// In-memory DA client for unit tests.  The inner store is reference-counted
/// so a clone of the handle gives access to the same submission log.
#[derive(Clone)]
pub struct MockDaClient {
    inner: Arc<Mutex<MockInner>>,
}

impl MockDaClient {
    /// Create a client that succeeds on every call.
    pub fn new() -> Self {
        MockDaClient {
            inner: Arc::new(Mutex::new(MockInner {
                submissions: vec![],
                fail: false,
            })),
        }
    }

    /// Create a client that always returns an error from `submit`.
    pub fn failing() -> Self {
        MockDaClient {
            inner: Arc::new(Mutex::new(MockInner {
                submissions: vec![],
                fail: true,
            })),
        }
    }

    /// Return all submissions seen so far.
    pub fn submissions(&self) -> Vec<MockSubmission> {
        self.inner.lock().unwrap().submissions.clone()
    }

    /// Number of successful submissions.
    pub fn submission_count(&self) -> usize {
        self.inner.lock().unwrap().submissions.len()
    }
}

impl Default for MockDaClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DataAvailabilityClient for MockDaClient {
    fn submit(&self, sequence: u64, data: &[u8]) -> anyhow::Result<DaReceipt> {
        let mut inner = self.inner.lock().unwrap();
        if inner.fail {
            anyhow::bail!(
                "MockDaClient: simulated DA failure for sequence {}",
                sequence
            );
        }
        let hash = keccak256(data);
        let receipt = DaReceipt {
            content_hash: hash,
            sequence,
        };
        inner.submissions.push(MockSubmission {
            sequence,
            data: data.to_vec(),
            receipt: receipt.clone(),
        });
        Ok(receipt)
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}
