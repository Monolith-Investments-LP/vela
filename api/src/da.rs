use sha3::{Digest, Keccak256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

pub trait DaClient: Send + Sync {
    fn submit(&self, sequence: u64, data: &[u8]) -> anyhow::Result<[u8; 32]>;
    fn name(&self) -> &'static str;
    fn retrieval_url(&self, sequence: u64) -> String;
}

pub struct LocalDaClient {
    dir: PathBuf,
}

impl LocalDaClient {
    pub fn new(dir: PathBuf) -> Self {
        LocalDaClient { dir }
    }
}

impl DaClient for LocalDaClient {
    fn submit(&self, sequence: u64, data: &[u8]) -> anyhow::Result<[u8; 32]> {
        std::fs::create_dir_all(&self.dir)?;
        let hash = keccak256(data);
        let filename = format!("da_order_{:016x}.bin", sequence);
        std::fs::write(self.dir.join(&filename), data)?;
        Ok(hash)
    }

    fn name(&self) -> &'static str {
        "local"
    }

    fn retrieval_url(&self, sequence: u64) -> String {
        format!(
            "file://{}/da_order_{:016x}.bin",
            self.dir.display(),
            sequence
        )
    }
}

pub struct DaSubmitter {
    client: Arc<dyn DaClient>,
    seq: AtomicU64,
}

impl DaSubmitter {
    pub fn new(client: Arc<dyn DaClient>) -> Self {
        DaSubmitter {
            client,
            seq: AtomicU64::new(0),
        }
    }

    pub fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn submit_order(&self, sequence: u64, data: &[u8]) -> anyhow::Result<(String, String)> {
        let hash = self.client.submit(sequence, data)?;
        let hash_hex = format!("0x{}", hex::encode(hash));
        let url = self.client.retrieval_url(sequence);
        Ok((hash_hex, url))
    }
}
