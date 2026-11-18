use super::*;
use crate::mcp::message::{ServerJsonRpcMessage, ServerResult};
use crate::mcp::model::NumberOrString;
use std::time::Duration;

/// GMCP-203-004-04 on a loaded runner: the session dropped `receive` (another
/// `select!` branch won) while its rejection waited for the writer an
/// answer still held, and the client never got its typed error. The
/// pipe here holds eight bytes, so the answer keeps the writer until
/// the client reads — the exact window, made deterministic.
#[tokio::test]
async fn a_dropped_receive_still_delivers_its_rejection() {
    let (mut client_in, server_in) = tokio::io::duplex(1024);
    let (server_out, client_out) = tokio::io::duplex(8);
    let mut gate = StdioGate::new(server_in, server_out);

    let answer = ServerJsonRpcMessage::response(ServerResult::Empty, NumberOrString::Number(1));
    let answering = tokio::spawn(gate.send(answer));
    while gate.write.try_lock().is_ok() {
        tokio::task::yield_now().await;
    }

    client_in.write_all(b"{not json\n").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), gate.receive())
            .await
            .is_err(),
        "nothing valid arrived, so receive must still be pending when it is dropped"
    );

    let reading = tokio::spawn(async move {
        let mut lines = BufReader::new(client_out).lines();
        let mut seen = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            seen.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        seen
    });
    answering.await.unwrap().unwrap();
    drop(client_in);
    assert!(gate.receive().await.is_none(), "EOF ends the session");
    gate.close().await.unwrap();
    drop(gate);

    let seen = reading.await.unwrap();
    assert!(
        seen.iter().any(|line| line["id"] == 1),
        "the answer must arrive: {seen:?}"
    );
    assert!(
        seen.iter().any(|line| line["error"]["code"] == -32700),
        "the dropped receive's rejection must still arrive: {seen:?}"
    );
}

/// Rejections leave in the order their lines arrived — a client that
/// pairs null-id errors with what it sent depends on it.
#[tokio::test]
async fn rejections_keep_their_order() {
    let (mut client_in, server_in) = tokio::io::duplex(1024);
    let (server_out, client_out) = tokio::io::duplex(1024);
    let mut gate = StdioGate::new(server_in, server_out);
    client_in.write_all(b"{not json\n[]\n").await.unwrap();
    drop(client_in);
    assert!(gate.receive().await.is_none());
    gate.close().await.unwrap();
    drop(gate);

    let mut lines = BufReader::new(client_out).lines();
    let mut codes = Vec::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        codes.push(serde_json::from_str::<Value>(&line).unwrap()["error"]["code"].clone());
    }
    assert_eq!(codes, [-32700, -32600]);
}

#[test]
fn classify_envelope_rejects_parse_errors_arrays_and_null_ids() {
    assert!(matches!(
        classify_envelope(b"{bad"),
        GateVerdict::Reject { code: -32700, .. }
    ));
    assert!(matches!(
        classify_envelope(b"[]"),
        GateVerdict::Reject { code: -32600, .. }
    ));
    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#),
        GateVerdict::Reject { code: -32600, .. }
    ));
    let no_id_task = br#"{"jsonrpc":"2.0","method":"tasks/get","params":{"taskId":"t-1"}}"#;
    assert!(matches!(
        classify_envelope(no_id_task),
        GateVerdict::Reject { code: -32600, .. }
    ));
    assert_eq!(
        http_envelope_rejection(no_id_task).and_then(|v| v["error"]["code"].as_i64()),
        Some(-32600)
    );
    assert!(http_envelope_rejection(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).is_none());
}

#[test]
fn classify_line_tracks_legacy_vs_modern_era_and_enforces_required_meta() {
    let mut legacy = EraState::default();
    let init_legacy =
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
    assert!(matches!(
        classify_line(init_legacy, &mut legacy),
        GateVerdict::Forward
    ));
    assert!(legacy.is_legacy());

    let mut modern = EraState::default();
    let discover = br#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#;
    assert!(matches!(
        classify_line(discover, &mut modern),
        GateVerdict::Forward
    ));
    assert!(modern.is_modern());
    let missing_meta = br#"{"jsonrpc":"2.0","id":2,"method":"ping","params":{}}"#;
    assert!(matches!(
        classify_line(missing_meta, &mut modern),
        GateVerdict::Reject { code: -32602, .. }
    ));
}

#[test]
fn classify_line_rejects_unsupported_modern_protocol_version_and_drops_custom_notifications() {
    let mut era = EraState::default();
    let bad_ver = br#"{"jsonrpc":"2.0","id":5,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1999-01-01","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    assert!(matches!(
        classify_line(bad_ver, &mut era),
        GateVerdict::Reject { code: -32022, .. }
    ));

    let custom_notif = br#"{"jsonrpc":"2.0","method":"notifications/custom_vendor_event"}"#;
    assert!(matches!(
        classify_line(custom_notif, &mut era),
        GateVerdict::Drop
    ));
}
