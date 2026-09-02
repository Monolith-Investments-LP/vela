//! Integration tests exercising the FIX 4.4 wire codec end-to-end.

use fix::{
    build_execution_report, build_heartbeat, build_logon, build_logout, msg_type, parse, serialize,
    tag, ExecutionReport, LogonMsg,
};

fn body_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\x01', "|")
}

#[test]
fn logon_round_trips() {
    let msg = build_logon(&LogonMsg {
        encrypt_method: 0,
        heart_bt_int_seconds: 30,
        reset_seq_num: false,
    });
    let bytes = serialize(&msg);
    // Human-friendly sanity check.
    let rendered = body_of(&bytes);
    assert!(rendered.starts_with("8=FIX.4.4|9="));
    assert!(rendered.contains("|35=A|"));
    let (parsed, consumed) = parse(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(parsed.msg_type(), Some(msg_type::LOGON));
    assert_eq!(parsed.get(tag::HEART_BT_INT), Some("30"));
}

#[test]
fn heartbeat_with_and_without_test_req_id() {
    let msg = build_heartbeat(None);
    let (parsed, _) = parse(&serialize(&msg)).unwrap();
    assert_eq!(parsed.msg_type(), Some(msg_type::HEARTBEAT));
    assert!(parsed.get(tag::TEST_REQ_ID).is_none());

    let msg = build_heartbeat(Some("ping-42"));
    let (parsed, _) = parse(&serialize(&msg)).unwrap();
    assert_eq!(parsed.get(tag::TEST_REQ_ID), Some("ping-42"));
}

#[test]
fn logout_carries_text() {
    let msg = build_logout(Some("shutting-down"));
    let (parsed, _) = parse(&serialize(&msg)).unwrap();
    assert_eq!(parsed.msg_type(), Some(msg_type::LOGOUT));
    assert_eq!(parsed.get(tag::TEXT), Some("shutting-down"));
}

#[test]
fn execution_report_new_ack_shape() {
    let msg = build_execution_report(&ExecutionReport {
        order_id: "V-1",
        cl_ord_id: "abc",
        exec_id: "E-1",
        exec_type: '0',
        ord_status: '0',
        symbol: "ETH-USDC",
        side: '1',
        leaves_qty: "1",
        cum_qty: "0",
        avg_px: "0",
        last_qty: None,
        last_px: None,
        transact_time_utc: "20260101-00:00:00.000",
        text: None,
    });
    let (parsed, _) = parse(&serialize(&msg)).unwrap();
    assert_eq!(parsed.msg_type(), Some(msg_type::EXECUTION_REPORT));
    assert_eq!(parsed.get(tag::ORDER_ID), Some("V-1"));
    assert_eq!(parsed.get(tag::CL_ORD_ID), Some("abc"));
    assert_eq!(parsed.get(tag::EXEC_TYPE), Some("0"));
    assert_eq!(parsed.get(tag::ORD_STATUS), Some("0"));
    assert_eq!(parsed.get(tag::LEAVES_QTY), Some("1"));
}
