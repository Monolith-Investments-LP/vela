//! Historical data export.
//!
//! Every quant desk backtests before allocating capital, and no capital
//! shows up on an exchange whose history you cannot download. This
//! module dumps the previous day's trades and one L2 snapshot per hour
//! to gzipped JSON-Lines files under an export directory. The bucket
//! layout mirrors Hyperliquid's public S3 structure so backtesting
//! tools (e.g. `hyperliquid-python-sdk` community loaders) work with
//! only a base-URL change.
//!
//! Output layout under `$VELA_EXPORT_DIR` (default `/data/exports`):
//!
//! ```text
//! trades/{market_id}/{yyyy-mm-dd}.jsonl.gz    — one line per fill
//! l2/{market_id}/{yyyy-mm-dd}.jsonl.gz        — hourly snapshots, one line each
//! ```
//!
//! The uploader is intentionally not part of v1: writing to a local
//! directory means any object-storage sync tool (rclone, `aws s3 sync`,
//! Cloudflare R2 client, etc.) can push the files without Vela needing
//! to hold provider credentials. When we want an in-process S3 push we
//! swap the `write_gzipped_jsonl` sink for one that streams to S3.

use anyhow::Result;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::types::StoredFill;
use crate::AppState;

/// Base directory for exports. Overridable via `VELA_EXPORT_DIR`.
fn export_dir() -> PathBuf {
    PathBuf::from(std::env::var("VELA_EXPORT_DIR").unwrap_or_else(|_| "/data/exports".to_string()))
}

/// Format a Unix millisecond timestamp as `YYYY-MM-DD` (UTC).
///
/// Deliberately dependency-free — this is called once per file per day,
/// not on the hot path, so a trivial arithmetic conversion beats
/// pulling in `chrono` for one use.
fn ymd_from_ms(ts_ms: u64) -> String {
    // Days since 1970-01-01
    let days = ts_ms / 86_400_000;
    // Zeller-like conversion; based on
    // https://howardhinnant.github.io/date_algorithms.html#civil_from_days
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Write a slice of serializable rows to `path` as newline-delimited
/// JSON, gzip-compressed. Creates parent directories as needed.
fn write_gzipped_jsonl<T: serde::Serialize>(path: &PathBuf, rows: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    for row in rows {
        let line = serde_json::to_string(row)?;
        encoder.write_all(line.as_bytes())?;
        encoder.write_all(b"\n")?;
    }
    encoder.finish()?;
    Ok(())
}

/// Snapshot of an order book at a point in time, for L2 dumps.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct L2Snapshot {
    timestamp_ms: u64,
    market_id: String,
    /// Top 50 bids and asks as `[price_fp, quantity_fp]` tuples. Uses
    /// visible quantity so iceberg reserves stay hidden in the dump.
    bids: Vec<[u64; 2]>,
    asks: Vec<[u64; 2]>,
}

/// Dump all fills that fall in `[from_ms, to_ms)` for the given target
/// day. Returns the number of files written and total rows.
pub async fn dump_trades_for_day(
    state: Arc<AppState>,
    from_ms: u64,
    to_ms: u64,
) -> Result<(usize, usize)> {
    let ymd = ymd_from_ms(from_ms);
    let base = export_dir();

    // Group fills by market_id under the lock, then release before I/O.
    let by_market: HashMap<String, Vec<StoredFill>> = {
        let fills = state.fills.lock().await;
        let mut m: HashMap<String, Vec<StoredFill>> = HashMap::new();
        for f in fills.iter() {
            if f.timestamp >= from_ms && f.timestamp < to_ms {
                m.entry(f.market_id.clone()).or_default().push(f.clone());
            }
        }
        m
    };

    let mut files = 0usize;
    let mut rows = 0usize;
    for (market_id, mut market_fills) in by_market {
        market_fills.sort_by_key(|f| f.timestamp);
        let path = base
            .join("trades")
            .join(&market_id)
            .join(format!("{}.jsonl.gz", ymd));
        write_gzipped_jsonl(&path, &market_fills)?;
        files += 1;
        rows += market_fills.len();
    }
    tracing::info!(
        "historical trade dump: day={} files={} rows={}",
        ymd,
        files,
        rows
    );
    Ok((files, rows))
}

/// Snapshot each market's L2 (top 50) and append one line per market to
/// today's `l2/{market_id}/{yyyy-mm-dd}.jsonl.gz`. Called once per hour.
pub async fn dump_l2_snapshots(state: Arc<AppState>, now_ms: u64) -> Result<usize> {
    let ymd = ymd_from_ms(now_ms);
    let base = export_dir();

    // Take snapshot under the engine lock; release before I/O.
    let snapshots: Vec<L2Snapshot> = {
        let engine = state.engine.lock().await;
        engine
            .order_books
            .iter()
            .map(|(mid, book)| L2Snapshot {
                timestamp_ms: now_ms,
                market_id: mid.0.clone(),
                bids: book
                    .depth_bids(50)
                    .into_iter()
                    .map(|(p, q)| [p, q])
                    .collect(),
                asks: book
                    .depth_asks(50)
                    .into_iter()
                    .map(|(p, q)| [p, q])
                    .collect(),
            })
            .collect()
    };

    // One file per market per day — append semantics via read+write. For
    // gzip this means read the existing file, decompress into memory,
    // append the new row, re-compress. Cheap because there are 24 rows
    // per market per day at most.
    for snap in &snapshots {
        let path = base
            .join("l2")
            .join(&snap.market_id)
            .join(format!("{}.jsonl.gz", ymd));

        let mut existing: Vec<L2Snapshot> = if path.exists() {
            let file = std::fs::File::open(&path)?;
            let mut decoder = flate2::read::GzDecoder::new(file);
            let mut text = String::new();
            std::io::Read::read_to_string(&mut decoder, &mut text)?;
            text.lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        } else {
            Vec::new()
        };
        existing.push(snap.clone());
        write_gzipped_jsonl(&path, &existing)?;
    }

    tracing::info!("historical L2 dump: markets={}", snapshots.len());
    Ok(snapshots.len())
}

/// Long-running task: dump yesterday's trades at ~00:05 UTC every day
/// and take an L2 snapshot every hour on the hour.
///
/// Skips entirely if `VELA_EXPORT_DIR` is set to the literal `disabled`.
pub async fn run_export_task(state: Arc<AppState>) {
    if std::env::var("VELA_EXPORT_DIR")
        .map(|v| v == "disabled")
        .unwrap_or(false)
    {
        tracing::info!("VELA_EXPORT_DIR=disabled — historical export task not started");
        return;
    }

    // Hourly L2 snapshots + daily trade rollup.
    let mut hourly = tokio::time::interval(Duration::from_secs(3600));
    hourly.tick().await; // Skip immediate first tick.

    loop {
        hourly.tick().await;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if let Err(e) = dump_l2_snapshots(Arc::clone(&state), now_ms).await {
            tracing::error!("L2 snapshot dump failed: {e}");
        }

        // On the first hour of each UTC day, roll up yesterday's trades.
        let ms_into_day = now_ms % 86_400_000;
        // Fire in the first hour after midnight UTC (0h < ms_into_day < 1h).
        if ms_into_day < 3_600_000 {
            let day_ms = 86_400_000u64;
            let start_of_today = now_ms - ms_into_day;
            let start_of_yesterday = start_of_today - day_ms;
            let state_clone = Arc::clone(&state);
            tokio::spawn(async move {
                if let Err(e) =
                    dump_trades_for_day(state_clone, start_of_yesterday, start_of_today).await
                {
                    tracing::error!("daily trade dump failed: {e}");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ymd_from_ms;

    #[test]
    fn ymd_epoch() {
        assert_eq!(ymd_from_ms(0), "1970-01-01");
    }

    #[test]
    fn ymd_known_dates() {
        // 2026-04-01 00:00:00 UTC = 1_775_001_600_000 ms
        assert_eq!(ymd_from_ms(1_775_001_600_000), "2026-04-01");
        // 2000-01-01 = 946_684_800_000 ms
        assert_eq!(ymd_from_ms(946_684_800_000), "2000-01-01");
        // 2024-02-29 (leap day) = 1_709_164_800_000 ms
        assert_eq!(ymd_from_ms(1_709_164_800_000), "2024-02-29");
    }

    #[test]
    fn ymd_across_day_boundary() {
        // 23:59:59.999 UTC on 2026-04-01
        let end_of_day = 1_775_001_600_000 + 86_399_999;
        assert_eq!(ymd_from_ms(end_of_day), "2026-04-01");
        // 00:00:00.000 UTC on 2026-04-02
        assert_eq!(ymd_from_ms(end_of_day + 1), "2026-04-02");
    }

    #[test]
    fn gzipped_jsonl_roundtrip() {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let dir = std::env::temp_dir().join(format!(
            "vela_hist_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jsonl.gz");

        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Row {
            a: u64,
            b: String,
        }
        let rows = vec![
            Row {
                a: 1,
                b: "x".to_string(),
            },
            Row {
                a: 2,
                b: "y".to_string(),
            },
            Row {
                a: 3,
                b: "z".to_string(),
            },
        ];
        super::write_gzipped_jsonl(&path, &rows).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut decoder = GzDecoder::new(file);
        let mut text = String::new();
        decoder.read_to_string(&mut text).unwrap();

        let parsed: Vec<Row> = text
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed, rows);

        std::fs::remove_dir_all(&dir).ok();
    }
}
