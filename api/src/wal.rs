use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use anyhow::Result;
use tokio::io::AsyncWriteExt;
use crc::{Crc, CRC_64_ECMA_182};

const CRC64: Crc<u64> = Crc::<u64>::new(&CRC_64_ECMA_182);

pub const ORDER_POST: u32 = 1;
pub const ORDER_PROCESSED: u32 = 2;
pub const ORDER_CANCEL: u32 = 3;
pub const FILL_CREATED: u32 = 4;
pub const DEPOSIT: u32 = 5;
pub const WITHDRAWAL_REQUEST: u32 = 6;
pub const BALANCE_CHANGE: u32 = 7;
pub const CHECKPOINT: u32 = 8;
pub const ENGINE_START: u32 = 9;
pub const ENGINE_STOP: u32 = 10;

const DEFAULT_MAX_SEGMENT_SIZE: u64 = 50 * 1024 * 1024;
const MAX_PAYLOAD_SIZE: u32 = 10 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
pub struct WalOrderPost {
    pub order_id: u64,
    pub user: String,
    pub market_id: String,
    pub side: String,
    pub price: u64,
    pub quantity: u64,
    pub order_type: String,
    pub time_in_force: String,
    pub nonce: u64,
    pub client_order_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct WalOrderProcessed {
    pub order_id: u64,
    pub result: String,
    pub fill_ids: Vec<String>,
    pub filled_quantity: u64,
    pub rejection_reason: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct WalOrderCancel {
    pub order_id: u64,
    pub client_order_id: Option<String>,
    pub user: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize)]
pub struct WalFillCreated {
    pub fill_id: String,
    pub market_id: String,
    pub maker_order_id: u64,
    pub taker_order_id: u64,
    pub maker_address: String,
    pub taker_address: String,
    pub price: u64,
    pub quantity: u64,
    pub maker_fee: i64,
    pub taker_fee: u64,
}

#[derive(Serialize, Deserialize)]
pub struct WalDeposit {
    pub user: String,
    pub asset: String,
    pub amount: u64,
    pub tx_hash: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct WalWithdrawalRequest {
    pub user: String,
    pub asset: String,
    pub amount: u64,
    pub nonce: u64,
}

#[derive(Serialize, Deserialize)]
pub struct WalBalanceChange {
    pub user: String,
    pub asset: String,
    pub old_balance: u64,
    pub new_balance: u64,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct WalCheckpoint {
    pub snapshot_path: String,
    pub total_orders: u64,
    pub total_fills: u64,
    pub total_users: u64,
}

#[derive(Serialize, Deserialize)]
pub struct WalEngineStart {
    pub version: String,
    pub reason: String,
    pub previous_sequence: u64,
}

#[derive(Serialize, Deserialize)]
pub struct WalEngineStop {
    pub reason: String,
    pub final_sequence: u64,
}

pub struct WalEntry {
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub entry_type: u32,
    pub payload_bytes: Vec<u8>,
}

impl WalEntry {
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        rmp_serde::from_slice(&self.payload_bytes)
            .map_err(|e| anyhow::anyhow!("WAL decode error: {e}"))
    }
}

pub struct WalStats {
    pub current_sequence: u64,
    pub current_segment: String,
    pub segment_size_bytes: u64,
    pub last_checkpoint_sequence: u64,
    pub last_checkpoint_time: u64,
    pub entries_since_checkpoint: u64,
    pub last_engine_start_reason: String,
}

struct WalInner {
    file: tokio::fs::File,
    segment_number: u32,
    segment_size: u64,
    last_checkpoint_sequence: u64,
    last_checkpoint_time_ms: u64,
    last_engine_start_reason: String,
}

pub struct Wal {
    dir: PathBuf,
    inner: tokio::sync::Mutex<WalInner>,
    sequence: Arc<AtomicU64>,
    #[allow(dead_code)]
    max_segment_size: u64,
}

fn segment_path(dir: &Path, num: u32) -> PathBuf {
    dir.join(format!("engine_wal_{:04}.log", num))
}

fn segment_name(num: u32) -> String {
    format!("engine_wal_{:04}.log", num)
}

fn parse_segment_number(name: &str) -> Option<u32> {
    name.strip_prefix("engine_wal_")
        .and_then(|s| s.strip_suffix(".log"))
        .and_then(|s| s.parse().ok())
}

fn find_max_segment_sync(dir: &Path) -> Option<u32> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            parse_segment_number(&name.to_string_lossy())
        })
        .max()
}

fn find_last_sequence_in_segment(path: &Path) -> u64 {
    Wal::read_from(path, 0)
        .unwrap_or_default()
        .last()
        .map(|e| e.sequence)
        .unwrap_or(0)
}

impl Wal {
    pub fn new(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;

        let segment_number = find_max_segment_sync(dir).unwrap_or(0);
        let path = segment_path(dir, segment_number);

        let existing_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        let last_seq = if existing_size > 0 {
            find_last_sequence_in_segment(&path)
        } else if segment_number > 0 {
            let prev_path = segment_path(dir, segment_number - 1);
            find_last_sequence_in_segment(&prev_path)
        } else {
            0
        };

        let std_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let file = tokio::fs::File::from_std(std_file);

        Ok(Self {
            dir: dir.to_path_buf(),
            inner: tokio::sync::Mutex::new(WalInner {
                file,
                segment_number,
                segment_size: existing_size,
                last_checkpoint_sequence: 0,
                last_checkpoint_time_ms: 0,
                last_engine_start_reason: "clean".to_string(),
            }),
            sequence: Arc::new(AtomicU64::new(last_seq + 1)),
            max_segment_size: DEFAULT_MAX_SEGMENT_SIZE,
        })
    }

    pub async fn open(dir: &Path) -> Result<Self> {
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || Self::new(&dir))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking: {e}"))?
    }

    pub async fn append<T: Serialize>(&self, entry_type: u32, payload: &T) -> Result<u64> {
        let payload_bytes = match rmp_serde::to_vec(payload) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("WAL serialize error: {e}");
                return Err(anyhow::anyhow!("WAL serialize error: {e}"));
            }
        };

        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);

        let ts_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let payload_len = payload_bytes.len() as u32;

        let mut header = [0u8; 24];
        header[0..8].copy_from_slice(&seq.to_be_bytes());
        header[8..16].copy_from_slice(&ts_ns.to_be_bytes());
        header[16..20].copy_from_slice(&entry_type.to_be_bytes());
        header[20..24].copy_from_slice(&payload_len.to_be_bytes());

        let mut data: Vec<u8> = Vec::with_capacity(24 + payload_bytes.len() + 8);
        data.extend_from_slice(&header);
        data.extend_from_slice(&payload_bytes);

        let checksum = CRC64.checksum(&data);
        data.extend_from_slice(&checksum.to_be_bytes());

        let entry_len = data.len() as u64;

        let mut inner = self.inner.lock().await;

        if let Err(e) = inner.file.write_all(&data).await {
            tracing::error!("WAL write error: {e}");
            return Err(e.into());
        }

        inner.segment_size += entry_len;

        if entry_type == CHECKPOINT {
            inner.last_checkpoint_sequence = seq;
            inner.last_checkpoint_time_ms = ts_ns / 1_000_000;
        }
        if entry_type == ENGINE_START {
            if let Ok(start) = rmp_serde::from_slice::<WalEngineStart>(&payload_bytes) {
                inner.last_engine_start_reason = start.reason;
            }
        }

        Ok(seq)
    }

    /// Flush all buffered writes to disk (one `sync_all` per call).
    /// Call once after all entries for a dispatch batch have been appended
    /// to amortise fsync cost across the entire batch.
    pub async fn flush(&self) -> Result<()> {
        #[allow(unused_mut)]
        let mut inner = self.inner.lock().await;
        if let Err(e) = inner.file.sync_all().await {
            tracing::error!("WAL flush error: {e}");
            return Err(e.into());
        }
        Ok(())
    }

    pub async fn rotate(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;

        let new_num = inner.segment_number + 1;
        let new_path = segment_path(&self.dir, new_num);

        let std_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)
            .map_err(|e| anyhow::anyhow!("WAL rotate open error: {e}"))?;

        inner.file = tokio::fs::File::from_std(std_file);
        inner.segment_number = new_num;
        inner.segment_size = 0;

        let oldest_to_keep = new_num.saturating_sub(2);
        let dir = self.dir.clone();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(num) = parse_segment_number(&name.to_string_lossy()) {
                    if num < oldest_to_keep {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }

        Ok(())
    }

    pub fn read_from(path: &Path, after_sequence: u64) -> Result<Vec<WalEntry>> {
        use std::io::Read;

        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };

        let mut entries = Vec::new();

        loop {
            let mut header = [0u8; 24];
            match file.read_exact(&mut header) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    tracing::warn!("WAL read header error: {e}");
                    break;
                }
            }

            let seq = u64::from_be_bytes(header[0..8].try_into().unwrap());
            let ts_ns = u64::from_be_bytes(header[8..16].try_into().unwrap());
            let entry_type = u32::from_be_bytes(header[16..20].try_into().unwrap());
            let payload_len = u32::from_be_bytes(header[20..24].try_into().unwrap());

            if payload_len > MAX_PAYLOAD_SIZE {
                tracing::warn!("WAL: suspicious payload length {} at seq {}, stopping", payload_len, seq);
                break;
            }

            let mut payload = vec![0u8; payload_len as usize];
            if let Err(e) = file.read_exact(&mut payload) {
                tracing::warn!("WAL: truncated payload at seq {}: {}", seq, e);
                break;
            }

            let mut crc_buf = [0u8; 8];
            if let Err(e) = file.read_exact(&mut crc_buf) {
                tracing::warn!("WAL: truncated CRC at seq {}: {}", seq, e);
                break;
            }
            let stored_crc = u64::from_be_bytes(crc_buf);

            let mut to_check: Vec<u8> = Vec::with_capacity(24 + payload_len as usize);
            to_check.extend_from_slice(&header);
            to_check.extend_from_slice(&payload);
            let computed_crc = CRC64.checksum(&to_check);

            if stored_crc != computed_crc {
                tracing::warn!("WAL: CRC mismatch at seq {}, skipping entry", seq);
                continue;
            }

            if seq > after_sequence {
                entries.push(WalEntry {
                    sequence: seq,
                    timestamp_ns: ts_ns,
                    entry_type,
                    payload_bytes: payload,
                });
            }
        }

        Ok(entries)
    }

    pub async fn find_last_checkpoint(dir: &Path) -> Result<Option<(u64, WalCheckpoint)>> {
        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut segments: Vec<u32> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name();
                    parse_segment_number(&name.to_string_lossy())
                })
                .collect();
            segments.sort_unstable();

            let mut last_checkpoint: Option<(u64, WalCheckpoint)> = None;
            for seg_num in segments {
                let path = segment_path(&dir, seg_num);
                let entries = Wal::read_from(&path, 0).unwrap_or_default();
                for entry in entries {
                    if entry.entry_type == CHECKPOINT {
                        if let Ok(cp) = entry.decode::<WalCheckpoint>() {
                            last_checkpoint = Some((entry.sequence, cp));
                        }
                    }
                }
            }
            Ok(last_checkpoint)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking: {e}"))?
    }

    pub fn was_clean_shutdown_sync(dir: &Path) -> bool {
        let max_seg = match find_max_segment_sync(dir) {
            Some(n) => n,
            None => return false,
        };

        let path = segment_path(dir, max_seg);
        let entries = Wal::read_from(&path, 0).unwrap_or_default();
        entries.iter().rev().any(|e| {
            if e.entry_type == ENGINE_STOP {
                if let Ok(stop) = e.decode::<WalEngineStop>() {
                    return stop.reason == "clean";
                }
            }
            false
        })
    }

    pub fn find_checkpoint_sequence_sync(dir: &Path) -> u64 {
        let mut segments: Vec<u32> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                parse_segment_number(&name.to_string_lossy())
            })
            .collect();
        segments.sort_unstable();

        let mut last_seq = 0u64;
        for seg_num in segments {
            let path = segment_path(dir, seg_num);
            let entries = Wal::read_from(&path, 0).unwrap_or_default();
            for entry in &entries {
                if entry.entry_type == CHECKPOINT {
                    last_seq = entry.sequence;
                }
            }
        }
        last_seq
    }

    pub fn wal_files_exist(dir: &Path) -> bool {
        find_max_segment_sync(dir).is_some()
    }

    pub fn current_sequence(&self) -> u64 {
        self.sequence.load(Ordering::SeqCst).saturating_sub(1)
    }

    pub async fn stats(&self) -> WalStats {
        let inner = self.inner.lock().await;
        let current_seq = self.sequence.load(Ordering::SeqCst).saturating_sub(1);
        let entries_since = current_seq.saturating_sub(inner.last_checkpoint_sequence);
        WalStats {
            current_sequence: current_seq,
            current_segment: segment_name(inner.segment_number),
            segment_size_bytes: inner.segment_size,
            last_checkpoint_sequence: inner.last_checkpoint_sequence,
            last_checkpoint_time: inner.last_checkpoint_time_ms,
            entries_since_checkpoint: entries_since,
            last_engine_start_reason: inner.last_engine_start_reason.clone(),
        }
    }
}
