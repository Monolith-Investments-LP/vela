use std::path::PathBuf;
use std::sync::Arc;

use api::wal::{
    Wal, WalCheckpoint, WalEngineStart, WalEngineStop, WalFillCreated, WalOrderPost,
    WalOrderProcessed, CHECKPOINT, ENGINE_START, ENGINE_STOP, FILL_CREATED, ORDER_POST,
    ORDER_PROCESSED,
};

/// Monotonic counter so two temp dirs created back-to-back in the same
/// test are guaranteed distinct even when the CI clock resolution is
/// coarser than one nanosecond (Linux runners with `CLOCK_REALTIME`
/// updated only every ~1ms occasionally serve the same value twice and
/// gave us a flaky `test_wal_clean_shutdown_detection`).
static TEMP_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_wal_dir(label: &str) -> PathBuf {
    let n = TEMP_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "vela_wal_{label}_{}_{n}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn test_wal_write_read_roundtrip() {
    let dir = temp_wal_dir("roundtrip");
    let wal = Arc::new(Wal::new(&dir).unwrap());

    let mut expected_seqs = Vec::new();

    let seq1 = wal
        .append(
            ORDER_POST,
            &WalOrderPost {
                order_id: 1,
                user: "0xabc".to_string(),
                market_id: "BTC-USDC".to_string(),
                side: "bid".to_string(),
                price: 50_000,
                quantity: 100,
                order_type: "limit".to_string(),
                time_in_force: "gtc".to_string(),
                nonce: 42,
                client_order_id: Some("coid-1".to_string()),
            },
        )
        .await
        .unwrap();
    expected_seqs.push(seq1);

    let seq2 = wal
        .append(
            ORDER_PROCESSED,
            &WalOrderProcessed {
                order_id: 1,
                result: "resting".to_string(),
                fill_ids: vec![],
                filled_quantity: 0,
                rejection_reason: None,
            },
        )
        .await
        .unwrap();
    expected_seqs.push(seq2);

    let seq3 = wal
        .append(
            FILL_CREATED,
            &WalFillCreated {
                fill_id: "fill_1_2".to_string(),
                market_id: "BTC-USDC".to_string(),
                maker_order_id: 1,
                taker_order_id: 2,
                maker_address: "0xmaker".to_string(),
                taker_address: "0xtaker".to_string(),
                price: 50_000,
                quantity: 100,
                maker_fee: -5,
                taker_fee: 25,
            },
        )
        .await
        .unwrap();
    expected_seqs.push(seq3);

    for i in 3u64..10 {
        let seq = wal
            .append(
                ENGINE_START,
                &WalEngineStart {
                    version: "0.2.0".to_string(),
                    reason: "clean".to_string(),
                    previous_sequence: i,
                },
            )
            .await
            .unwrap();
        expected_seqs.push(seq);
    }

    let seg_path = dir.join("engine_wal_0000.log");
    let entries = Wal::read_from(&seg_path, 0).unwrap();

    assert_eq!(entries.len(), 10, "expected 10 entries");
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.sequence, expected_seqs[i],
            "sequence mismatch at index {i}"
        );
    }

    let post: WalOrderPost = entries[0].decode().unwrap();
    assert_eq!(post.order_id, 1);
    assert_eq!(post.market_id, "BTC-USDC");
    assert_eq!(post.client_order_id, Some("coid-1".to_string()));

    let processed: WalOrderProcessed = entries[1].decode().unwrap();
    assert_eq!(processed.result, "resting");

    let fill: WalFillCreated = entries[2].decode().unwrap();
    assert_eq!(fill.fill_id, "fill_1_2");
    assert_eq!(fill.maker_fee, -5);
}

#[tokio::test]
async fn test_wal_crc_corruption_skip() {
    let dir = temp_wal_dir("crc");
    let wal = Arc::new(Wal::new(&dir).unwrap());

    wal.append(
        ORDER_POST,
        &WalOrderPost {
            order_id: 10,
            user: "0xa".to_string(),
            market_id: "ETH-USDC".to_string(),
            side: "ask".to_string(),
            price: 3000,
            quantity: 1,
            order_type: "limit".to_string(),
            time_in_force: "gtc".to_string(),
            nonce: 1,
            client_order_id: None,
        },
    )
    .await
    .unwrap();

    wal.append(
        ORDER_PROCESSED,
        &WalOrderProcessed {
            order_id: 10,
            result: "resting".to_string(),
            fill_ids: vec![],
            filled_quantity: 0,
            rejection_reason: None,
        },
    )
    .await
    .unwrap();

    wal.append(
        ENGINE_STOP,
        &WalEngineStop {
            reason: "clean".to_string(),
            final_sequence: 99,
        },
    )
    .await
    .unwrap();

    drop(wal);

    let seg_path = dir.join("engine_wal_0000.log");
    let file_len = std::fs::metadata(&seg_path).unwrap().len();
    assert!(file_len > 60, "file too small: {file_len}");

    let mut raw = std::fs::read(&seg_path).unwrap();
    let corrupt_offset = (file_len as usize) / 3;
    raw[corrupt_offset] ^= 0xFF;
    std::fs::write(&seg_path, &raw).unwrap();

    let entries = Wal::read_from(&seg_path, 0).unwrap();
    assert!(
        !entries.is_empty(),
        "at least one valid entry should be readable after corruption"
    );
    let total_valid = entries.len();
    assert!(total_valid < 3, "corrupted entry should be skipped");
    assert!(total_valid >= 1, "at least one entry should be readable");
}

#[tokio::test]
async fn test_wal_segment_rotation() {
    let dir = temp_wal_dir("rotation");
    let small_max = 512u64;

    let wal = Wal::new(&dir).unwrap();

    let large_payload = WalOrderPost {
        order_id: 0,
        user: "0x".to_string() + &"a".repeat(100),
        market_id: "BTC-USDC".to_string(),
        side: "bid".to_string(),
        price: 1,
        quantity: 1,
        order_type: "limit".to_string(),
        time_in_force: "gtc".to_string(),
        nonce: 0,
        client_order_id: None,
    };

    for i in 0u64..20 {
        let mut entry = large_payload.clone();
        entry.order_id = i;
        entry.nonce = i;
        wal.append(ORDER_POST, &entry).await.unwrap();
    }

    wal.append(
        CHECKPOINT,
        &WalCheckpoint {
            snapshot_path: "/data/snap.json".to_string(),
            total_orders: 20,
            total_fills: 0,
            total_users: 1,
        },
    )
    .await
    .unwrap();

    wal.rotate().await.unwrap();

    let seg1_exists = dir.join("engine_wal_0001.log").exists();
    assert!(seg1_exists, "segment 1 should exist after rotation");

    for i in 20u64..25 {
        let mut entry = large_payload.clone();
        entry.order_id = i;
        entry.nonce = i;
        wal.append(ORDER_POST, &entry).await.unwrap();
    }

    let seg0_entries = Wal::read_from(&dir.join("engine_wal_0000.log"), 0).unwrap();
    assert!(
        !seg0_entries.is_empty(),
        "segment 0 should still be readable"
    );

    let seg1_entries = Wal::read_from(&dir.join("engine_wal_0001.log"), 0).unwrap();
    assert_eq!(seg1_entries.len(), 5, "segment 1 should have 5 entries");
}

#[tokio::test]
async fn test_wal_replay_recovery() {
    let dir = temp_wal_dir("recovery");

    let wal = Arc::new(Wal::new(&dir).unwrap());

    wal.append(
        CHECKPOINT,
        &WalCheckpoint {
            snapshot_path: "/data/snap.json".to_string(),
            total_orders: 0,
            total_fills: 0,
            total_users: 0,
        },
    )
    .await
    .unwrap();

    let checkpoint_seq = wal.current_sequence();

    let post_entries = [
        (100u64, "0xuser1", 50_000u64, 1u64),
        (101, "0xuser2", 51_000, 2),
        (102, "0xuser3", 52_000, 3),
        (103, "0xuser4", 53_000, 4),
        (104, "0xuser5", 54_000, 5),
    ];

    for (order_id, user, price, nonce) in &post_entries {
        wal.append(
            ORDER_POST,
            &WalOrderPost {
                order_id: *order_id,
                user: user.to_string(),
                market_id: "BTC-USDC".to_string(),
                side: "bid".to_string(),
                price: *price,
                quantity: 100,
                order_type: "limit".to_string(),
                time_in_force: "gtc".to_string(),
                nonce: *nonce,
                client_order_id: None,
            },
        )
        .await
        .unwrap();

        wal.append(
            ORDER_PROCESSED,
            &WalOrderProcessed {
                order_id: *order_id,
                result: "resting".to_string(),
                fill_ids: vec![],
                filled_quantity: 0,
                rejection_reason: None,
            },
        )
        .await
        .unwrap();
    }

    drop(wal);

    let seg_path = dir.join("engine_wal_0000.log");
    let all_entries = Wal::read_from(&seg_path, checkpoint_seq).unwrap();
    assert_eq!(
        all_entries.len(),
        10,
        "5 ORDER_POST + 5 ORDER_PROCESSED = 10 entries after checkpoint"
    );

    let post_count = all_entries
        .iter()
        .filter(|e| e.entry_type == ORDER_POST)
        .count();
    assert_eq!(post_count, 5, "should have 5 ORDER_POST entries");

    let processed_count = all_entries
        .iter()
        .filter(|e| e.entry_type == ORDER_PROCESSED)
        .count();
    assert_eq!(processed_count, 5, "should have 5 ORDER_PROCESSED entries");

    let seen_ids: Vec<u64> = all_entries
        .iter()
        .filter(|e| e.entry_type == ORDER_POST)
        .filter_map(|e| e.decode::<WalOrderPost>().ok().map(|p| p.order_id))
        .collect();

    for (expected_id, _, _, _) in &post_entries {
        assert!(
            seen_ids.contains(expected_id),
            "order {expected_id} should be in replay"
        );
    }
}

#[tokio::test]
async fn test_wal_clean_shutdown_detection() {
    let dir = temp_wal_dir("clean_shutdown");
    let wal = Arc::new(Wal::new(&dir).unwrap());

    assert!(
        !Wal::was_clean_shutdown_sync(&dir),
        "no ENGINE_STOP yet — not clean"
    );

    wal.append(
        ENGINE_START,
        &WalEngineStart {
            version: "0.2.0".to_string(),
            reason: "clean".to_string(),
            previous_sequence: 0,
        },
    )
    .await
    .unwrap();

    assert!(!Wal::was_clean_shutdown_sync(&dir), "still no ENGINE_STOP");

    wal.append(
        ENGINE_STOP,
        &WalEngineStop {
            reason: "clean".to_string(),
            final_sequence: wal.current_sequence(),
        },
    )
    .await
    .unwrap();

    // Explicit flush before the sync read — otherwise the ENGINE_STOP
    // may still be in the tokio file buffer when
    // `was_clean_shutdown_sync` opens the file with std::fs, which
    // showed up as a flake on Linux runners even though the drop below
    // eventually closes the handle.
    wal.flush().await.unwrap();
    drop(wal);

    assert!(
        Wal::was_clean_shutdown_sync(&dir),
        "ENGINE_STOP with reason=clean should be detected"
    );

    let dir2 = temp_wal_dir("unclean_shutdown");
    let wal2 = Arc::new(Wal::new(&dir2).unwrap());

    wal2.append(
        ENGINE_START,
        &WalEngineStart {
            version: "0.2.0".to_string(),
            reason: "clean".to_string(),
            previous_sequence: 0,
        },
    )
    .await
    .unwrap();

    wal2.flush().await.unwrap();
    drop(wal2);

    assert!(
        !Wal::was_clean_shutdown_sync(&dir2),
        "no ENGINE_STOP should be detected as unclean"
    );
}
