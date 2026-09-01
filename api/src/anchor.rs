use crate::types::AnchorRecord;
use crate::AppState;
use k256::ecdsa::SigningKey;
use sha3::{Digest, Keccak256};
use std::sync::atomic::Ordering;
use std::sync::Arc;

const CHAIN_ID: u64 = 11155111;
const GAS_LIMIT: u64 = 100_000;

fn rlp_uint(value: u128) -> Vec<u8> {
    if value == 0 {
        return rlp_bytes(&[]);
    }
    let bytes = value.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(15);
    rlp_bytes(&bytes[start..])
}

fn rlp_u64(value: u64) -> Vec<u8> {
    rlp_uint(value as u128)
}

fn rlp_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() == 1 && data[0] < 0x80 {
        return data.to_vec();
    }
    if data.len() <= 55 {
        let mut out = vec![0x80 + data.len() as u8];
        out.extend_from_slice(data);
        out
    } else {
        let len_bytes = encode_len(data.len());
        let mut out = vec![0xb7 + len_bytes.len() as u8];
        out.extend_from_slice(&len_bytes);
        out.extend_from_slice(data);
        out
    }
}

fn rlp_list(items: Vec<Vec<u8>>) -> Vec<u8> {
    let payload: Vec<u8> = items.into_iter().flatten().collect();
    if payload.len() <= 55 {
        let mut out = vec![0xc0 + payload.len() as u8];
        out.extend_from_slice(&payload);
        out
    } else {
        let len_bytes = encode_len(payload.len());
        let mut out = vec![0xf7 + len_bytes.len() as u8];
        out.extend_from_slice(&len_bytes);
        out.extend_from_slice(&payload);
        out
    }
}

fn encode_len(len: usize) -> Vec<u8> {
    let bytes = (len as u64).to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    bytes[start..].to_vec()
}

fn encode_anchor_calldata(state_root: [u8; 32], orders_processed: u64) -> Vec<u8> {
    let mut h = Keccak256::new();
    h.update(b"anchorStateRoot(bytes32,uint256)");
    let hash: [u8; 32] = h.finalize().into();

    let mut data = Vec::with_capacity(68);
    data.extend_from_slice(&hash[..4]);
    data.extend_from_slice(&state_root);

    let mut op_bytes = [0u8; 32];
    op_bytes[24..].copy_from_slice(&orders_processed.to_be_bytes());
    data.extend_from_slice(&op_bytes);
    data
}

fn operator_address(key_bytes: &[u8]) -> Result<[u8; 20], String> {
    let signing_key = SigningKey::from_slice(key_bytes).map_err(|e| e.to_string())?;
    let verifying_key = signing_key.verifying_key();
    let point = verifying_key.to_encoded_point(false);
    let pub_bytes = point.as_bytes();

    let mut h = Keccak256::new();
    h.update(&pub_bytes[1..]);
    let hash: [u8; 32] = h.finalize().into();

    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[12..]);
    Ok(addr)
}

async fn eth_rpc(
    url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1,
    });
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(err) = json.get("error") {
        return Err(format!("RPC error: {err}"));
    }
    Ok(json["result"].clone())
}

async fn send_anchor_tx(
    alchemy_url: &str,
    operator_key_hex: &str,
    state_root: [u8; 32],
    orders_processed: u64,
) -> Result<String, String> {
    let key_hex = operator_key_hex
        .strip_prefix("0x")
        .unwrap_or(operator_key_hex);
    let key_bytes = hex::decode(key_hex).map_err(|_| "invalid operator key".to_string())?;
    let signing_key = SigningKey::from_slice(&key_bytes).map_err(|e| e.to_string())?;
    let op_addr = operator_address(&key_bytes)?;
    let op_addr_hex = format!("0x{}", hex::encode(op_addr));

    let nonce_val = eth_rpc(
        alchemy_url,
        "eth_getTransactionCount",
        serde_json::json!([op_addr_hex, "pending"]),
    )
    .await?;
    let nonce_str = nonce_val.as_str().ok_or("bad nonce response")?;
    let nonce = u64::from_str_radix(nonce_str.strip_prefix("0x").unwrap_or(nonce_str), 16)
        .map_err(|_| "failed to parse nonce")?;

    let gas_price_val = eth_rpc(alchemy_url, "eth_gasPrice", serde_json::json!([])).await;
    let base_gas: u128 = match gas_price_val {
        Ok(v) => {
            let s = v.as_str().unwrap_or("0x77359400");
            u128::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).unwrap_or(2_000_000_000)
        }
        Err(_) => 2_000_000_000u128,
    };
    let max_fee_per_gas = base_gas.saturating_mul(3).max(3_000_000_000);
    let max_priority_fee_per_gas = 2_000_000_000u128.min(max_fee_per_gas);

    let contract_addr_str = std::env::var("VELA_CONTRACT_ADDRESS")
        .unwrap_or_else(|_| "0xAa8E680c11a883F9bf6eb980B2D4E9D18DD25686".to_string());
    let contract_hex = contract_addr_str
        .strip_prefix("0x")
        .unwrap_or(&contract_addr_str);
    let contract_bytes = hex::decode(contract_hex).map_err(|_| "bad contract address")?;
    if contract_bytes.len() != 20 {
        return Err("contract address must be 20 bytes".to_string());
    }

    let data = encode_anchor_calldata(state_root, orders_processed);

    let pre_sign = {
        let mut out = vec![0x02u8];
        let items = vec![
            rlp_u64(CHAIN_ID),
            rlp_u64(nonce),
            rlp_uint(max_priority_fee_per_gas),
            rlp_uint(max_fee_per_gas),
            rlp_u64(GAS_LIMIT),
            rlp_bytes(&contract_bytes),
            rlp_uint(0u128),
            rlp_bytes(&data),
            rlp_list(vec![]),
        ];
        out.extend_from_slice(&rlp_list(items));
        out
    };

    let sign_hash: [u8; 32] = {
        let mut h = Keccak256::new();
        h.update(&pre_sign);
        h.finalize().into()
    };

    let (sig, recid) = signing_key
        .sign_prehash_recoverable(&sign_hash)
        .map_err(|e| e.to_string())?;

    let sig_bytes = sig.to_bytes();
    let y_parity = recid.to_byte() as u64;

    let mut r_bytes = [0u8; 32];
    let mut s_bytes = [0u8; 32];
    r_bytes.copy_from_slice(&sig_bytes[..32]);
    s_bytes.copy_from_slice(&sig_bytes[32..]);

    let signed_tx = {
        let mut out = vec![0x02u8];
        let items = vec![
            rlp_u64(CHAIN_ID),
            rlp_u64(nonce),
            rlp_uint(max_priority_fee_per_gas),
            rlp_uint(max_fee_per_gas),
            rlp_u64(GAS_LIMIT),
            rlp_bytes(&contract_bytes),
            rlp_uint(0u128),
            rlp_bytes(&data),
            rlp_list(vec![]),
            rlp_u64(y_parity),
            rlp_bytes(&r_bytes),
            rlp_bytes(&s_bytes),
        ];
        out.extend_from_slice(&rlp_list(items));
        out
    };

    let raw_hex = format!("0x{}", hex::encode(&signed_tx));
    let result = eth_rpc(
        alchemy_url,
        "eth_sendRawTransaction",
        serde_json::json!([raw_hex]),
    )
    .await?;

    let tx_hash = result.as_str().ok_or("no tx hash in response")?.to_string();
    Ok(tx_hash)
}

async fn compute_state_root(state: &Arc<AppState>) -> ([u8; 32], u64) {
    let fills = state.fills.lock().await;
    let fill_ids: Vec<String> = fills.iter().map(|f| f.id.clone()).collect();
    drop(fills);

    let orders = state.stored_orders.lock().await;
    let order_ids: Vec<String> = orders.keys().map(|k| k.to_string()).collect();
    let orders_processed = orders.len() as u64;
    drop(orders);

    let mut h = Keccak256::new();
    for id in &fill_ids {
        h.update(id.as_bytes());
    }
    for id in &order_ids {
        h.update(id.as_bytes());
    }
    let root: [u8; 32] = h.finalize().into();
    (root, orders_processed)
}

fn anchors_path() -> String {
    let dir = std::env::var("SNAPSHOT_DIR").unwrap_or_else(|_| "/data".to_string());
    format!("{dir}/anchors.json")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AnchorsPersisted {
    count: u64,
    anchors: Vec<AnchorRecord>,
}

pub async fn load_anchors() -> Option<(Vec<AnchorRecord>, u64)> {
    let path = anchors_path();
    let data = tokio::fs::read(&path).await.ok()?;
    let p: AnchorsPersisted = serde_json::from_slice(&data).ok()?;
    Some((p.anchors, p.count))
}

async fn save_anchors(anchors: &[AnchorRecord], count: u64) -> anyhow::Result<()> {
    let path = anchors_path();
    let p = AnchorsPersisted {
        count,
        anchors: anchors.to_vec(),
    };
    let json = serde_json::to_vec(&p)?;
    let tmp = format!("{path}.tmp");
    tokio::fs::write(&tmp, &json).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

pub async fn anchor_task(state: Arc<AppState>, alchemy_url: String, operator_key: String) {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;

        let (state_root_bytes, orders_processed) = compute_state_root(&state).await;
        let state_root_hex = format!("0x{}", hex::encode(state_root_bytes));

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        match send_anchor_tx(
            &alchemy_url,
            &operator_key,
            state_root_bytes,
            orders_processed,
        )
        .await
        {
            Ok(tx_hash) => {
                let anchor_id = state.anchor_count.fetch_add(1, Ordering::Relaxed);

                let record = AnchorRecord {
                    anchor_id,
                    state_root: state_root_hex,
                    tx_hash: tx_hash.clone(),
                    timestamp: now_ms,
                    orders_processed,
                    block_number: None,
                };

                {
                    let mut anchors = state.anchors.lock().await;
                    anchors.push(record);
                    let len = anchors.len();
                    if len > 100 {
                        anchors.drain(0..len - 100);
                    }
                }

                *state.last_anchor_tx.lock().await = Some(tx_hash.clone());
                state.last_anchor_time.store(now_ms, Ordering::Relaxed);

                tracing::info!("State root anchored: anchor_id={anchor_id} tx={tx_hash}");

                let anchors_snap = state.anchors.lock().await.clone();
                let count = state.anchor_count.load(Ordering::Relaxed);
                if let Err(e) = save_anchors(&anchors_snap, count).await {
                    tracing::error!("Failed to persist anchors: {e}");
                }
            }
            Err(e) => {
                tracing::error!("Anchor task error: {e}");
            }
        }
    }
}
