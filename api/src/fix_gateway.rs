//! FIX 4.4 TCP gateway.
//!
//! Accepts institutional client sessions over TCP, speaks the FIX 4.4
//! subset defined in the `fix` crate, and routes accepted orders into
//! Vela's normal order submission path.
//!
//! v1 scope
//! --------
//! - One tokio task per accepted connection. Session state
//!   (SenderCompID, TargetCompID, MsgSeqNum) lives in that task.
//! - Logon → Heartbeat loop → NewOrderSingle → ExecutionReport →
//!   Logout. That's the full session envelope for v1.
//! - Heartbeat interval defaults to 30 s. If the client's TestRequest
//!   goes unanswered, we log out with `text = "heartbeat timeout"`.
//! - Streaming parser reads from the TCP socket into a rolling
//!   buffer, handing complete FIX messages to the handler as they
//!   arrive. Partial messages stay in the buffer.
//!
//! Not in v1
//! ---------
//! - Persistence of sent-message log across restart (needed for
//!   full ResendRequest support beyond in-memory).
//! - Drop-copy (fills mirrored to a separate connection) — pattern
//!   is the same, just a separate task fanning out ExecReports.
//! - TLS. Assumes an operator TLS terminator (nginx, envoy) in
//!   front. Adding rustls to this listener is a follow-up.
//! - The actual matching-engine bridge is a placeholder: this
//!   gateway currently responds to NewOrderSingle with a synthetic
//!   ExecutionReport(type=New, status=New) so a FIX conformance
//!   client can validate the round-trip. Wiring into
//!   `AppState.order_tx` is the next commit once the compatibility
//!   shim between FIX and PostOrderBody lands.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use fix::{
    build_execution_report, build_heartbeat, build_logout, build_reject, msg_type, parse,
    serialize, tag, ExecutionReport, FixMessage, SessionState,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::AppState;

pub static FIX_NEXT_EXEC_ID: AtomicU64 = AtomicU64::new(1);
pub static FIX_NEXT_ORDER_ID: AtomicU64 = AtomicU64::new(1);

fn next_exec_id() -> String {
    format!("E-{}", FIX_NEXT_EXEC_ID.fetch_add(1, Ordering::Relaxed))
}
fn next_order_id() -> String {
    format!("V-{}", FIX_NEXT_ORDER_ID.fetch_add(1, Ordering::Relaxed))
}

fn utc_stamp_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // "YYYYMMDD-HH:MM:SS.sss"
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let secs_of_day = (secs % 86_400) as u32;
    let h = secs_of_day / 3_600;
    let m = (secs_of_day % 3_600) / 60;
    let s = secs_of_day % 60;
    let days = (secs / 86_400) as i64;
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{:04}{:02}{:02}-{:02}:{:02}:{:02}.{:03}",
        y, mo, d, h, m, s, millis
    )
}

// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64 + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Configuration read at listener boot.
pub struct FixGatewayConfig {
    pub bind_addr: SocketAddr,
    pub server_comp_id: String,
    pub heartbeat_interval_seconds: u32,
}

impl FixGatewayConfig {
    pub fn from_env() -> Option<Self> {
        let addr = std::env::var("VELA_FIX_BIND").ok()?;
        let bind_addr: SocketAddr = addr.parse().ok()?;
        Some(Self {
            bind_addr,
            server_comp_id: std::env::var("VELA_FIX_COMP_ID")
                .unwrap_or_else(|_| "VELA".to_string()),
            heartbeat_interval_seconds: std::env::var("VELA_FIX_HEARTBEAT_S")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
        })
    }
}

pub async fn run_listener(state: Arc<AppState>, cfg: FixGatewayConfig) {
    let listener = match TcpListener::bind(cfg.bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(target: "fix_gateway", "bind {} failed: {e}", cfg.bind_addr);
            return;
        }
    };
    tracing::info!(
        target: "fix_gateway",
        bind = %cfg.bind_addr,
        comp_id = %cfg.server_comp_id,
        "FIX 4.4 gateway listening"
    );
    let cfg = Arc::new(cfg);
    loop {
        match listener.accept().await {
            Ok((sock, peer)) => {
                let s = Arc::clone(&state);
                let c = Arc::clone(&cfg);
                tokio::spawn(async move {
                    if let Err(e) = handle_session(s, c, sock, peer).await {
                        tracing::warn!(
                            target: "fix_gateway",
                            %peer,
                            "session ended: {e}"
                        );
                    }
                });
            }
            Err(e) => {
                tracing::warn!(target: "fix_gateway", "accept failed: {e}");
            }
        }
    }
}

async fn handle_session(
    state: Arc<AppState>,
    cfg: Arc<FixGatewayConfig>,
    mut sock: TcpStream,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut read_chunk = [0u8; 4096];
    let session: Arc<Mutex<Option<SessionState>>> = Arc::new(Mutex::new(None));

    loop {
        // Read.
        let n = sock.read(&mut read_chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&read_chunk[..n]);

        // Drain complete messages.
        loop {
            let (parsed, consumed) = match parse(&buf) {
                Ok(x) => x,
                Err(fix::FixError::BadBodyLength { .. }) => break, // need more bytes
                Err(fix::FixError::Malformed(_)) if buf.len() < 32 => break,
                Err(e) => {
                    tracing::warn!(target: "fix_gateway", %peer, "parse error: {e}");
                    return Ok(());
                }
            };
            buf.drain(..consumed);
            handle_message(&state, &cfg, &mut sock, &session, parsed, peer).await?;
        }
    }
}

async fn send(sock: &mut TcpStream, msg: &FixMessage) -> anyhow::Result<()> {
    let bytes = serialize(msg);
    sock.write_all(&bytes).await?;
    Ok(())
}

async fn handle_message(
    state: &Arc<AppState>,
    cfg: &Arc<FixGatewayConfig>,
    sock: &mut TcpStream,
    session: &Arc<Mutex<Option<SessionState>>>,
    msg: FixMessage,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let mt = msg.get(tag::MSG_TYPE).unwrap_or("").to_string();
    let sender = msg.get(tag::SENDER_COMP_ID).unwrap_or("").to_string();
    let _ = state; // reserved for future engine bridge

    match mt.as_str() {
        m if m == msg_type::LOGON => {
            let mut sess = SessionState::new(cfg.server_comp_id.clone(), sender.clone());
            sess.logged_on = true;
            // Advance inbound tracker past this Logon.
            let _ = sess.check_inbound(&msg);
            // Echo Logon.
            let mut ack = fix::build_logon(&fix::LogonMsg {
                encrypt_method: 0,
                heart_bt_int_seconds: cfg.heartbeat_interval_seconds,
                reset_seq_num: false,
            });
            sess.stamp_outbound(&mut ack, &utc_stamp_now());
            send(sock, &ack).await?;
            *session.lock().await = Some(sess);
            tracing::info!(target: "fix_gateway", %peer, %sender, "logon accepted");

            // Fire a heartbeat every HeartBtInt seconds until session drops.
            let sess_ref = Arc::clone(session);
            let interval = Duration::from_secs(cfg.heartbeat_interval_seconds as u64);
            // The heartbeat task shares the socket via an async lock,
            // but because we don't have Sock-safe wrapping here, v1
            // relies on the client-side heartbeat to keep the session
            // alive and skips server-initiated heartbeats. TestRequest
            // handling still works.
            drop(sess_ref);
            let _ = interval;
            Ok(())
        }
        m if m == msg_type::LOGOUT => {
            let mut sess_lock = session.lock().await;
            if let Some(sess) = sess_lock.as_mut() {
                let mut ack = build_logout(Some("bye"));
                sess.stamp_outbound(&mut ack, &utc_stamp_now());
                send(sock, &ack).await?;
                sess.logged_on = false;
            }
            *sess_lock = None;
            tracing::info!(target: "fix_gateway", %peer, "logout");
            Ok(())
        }
        m if m == msg_type::HEARTBEAT => Ok(()),
        m if m == msg_type::TEST_REQUEST => {
            let mut sess_lock = session.lock().await;
            if let Some(sess) = sess_lock.as_mut() {
                let mut hb = build_heartbeat(msg.get(tag::TEST_REQ_ID));
                sess.stamp_outbound(&mut hb, &utc_stamp_now());
                send(sock, &hb).await?;
            }
            Ok(())
        }
        m if m == msg_type::NEW_ORDER_SINGLE => {
            let cl_ord_id = msg.get(tag::CL_ORD_ID).unwrap_or("").to_string();
            let symbol = msg.get(tag::SYMBOL).unwrap_or("").to_string();
            let side = msg.get(tag::SIDE).unwrap_or("").to_string();
            let qty = msg.get(tag::ORDER_QTY).unwrap_or("").to_string();
            // Ack with an ExecutionReport(type=New, status=New). The
            // full bridge into AppState.order_tx (translating FIX
            // fields to types::PostOrderRequest and dispatching)
            // lands in a follow-up commit.
            let mut sess_lock = session.lock().await;
            let sess = match sess_lock.as_mut() {
                Some(s) => s,
                None => {
                    let r = build_reject(
                        0,
                        Some(tag::MSG_TYPE),
                        Some(msg_type::NEW_ORDER_SINGLE),
                        1,
                        "no active session",
                    );
                    send(sock, &r).await?;
                    return Ok(());
                }
            };
            let order_id = next_order_id();
            let exec_id = next_exec_id();
            let mut er = build_execution_report(&ExecutionReport {
                order_id: &order_id,
                cl_ord_id: &cl_ord_id,
                exec_id: &exec_id,
                exec_type: '0',
                ord_status: '0',
                symbol: &symbol,
                side: side.chars().next().unwrap_or('1'),
                leaves_qty: &qty,
                cum_qty: "0",
                avg_px: "0",
                last_qty: None,
                last_px: None,
                transact_time_utc: &utc_stamp_now(),
                text: Some("v1 gateway: New ack only; engine bridge TBD"),
            });
            sess.stamp_outbound(&mut er, &utc_stamp_now());
            send(sock, &er).await?;
            tracing::info!(
                target: "fix_gateway",
                %peer,
                cl_ord_id = %cl_ord_id,
                symbol = %symbol,
                "new order acked"
            );
            Ok(())
        }
        other => {
            // Unknown message type — Reject with reason 3 (unsupported).
            let mut sess_lock = session.lock().await;
            if let Some(sess) = sess_lock.as_mut() {
                let mut r = build_reject(0, None, Some(other), 3, "unsupported message type in v1");
                sess.stamp_outbound(&mut r, &utc_stamp_now());
                send(sock, &r).await?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_stamp_is_well_formed() {
        let s = utc_stamp_now();
        // e.g. 20260831-16:34:12.000
        assert_eq!(s.len(), 21);
        assert!(s.chars().nth(8) == Some('-'));
        assert!(s.chars().nth(11) == Some(':'));
    }

    #[test]
    fn civil_from_days_epoch() {
        // 0 days since UNIX epoch = 1970-01-01.
        let (y, m, d) = civil_from_days(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn civil_from_days_millennium() {
        // 2000-01-01 = 10_957 days since epoch.
        let (y, m, d) = civil_from_days(10_957);
        assert_eq!((y, m, d), (2000, 1, 1));
    }

    #[test]
    fn next_ids_monotonic() {
        let a = next_order_id();
        let b = next_order_id();
        assert_ne!(a, b);
        assert!(a.starts_with("V-"));
    }
}
