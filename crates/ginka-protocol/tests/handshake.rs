//! The handshake: version, token, and the epoch that tells a reconnecting
//! client whether it is talking to the same daemon run it left.

use ginka_protocol::envelope::{
    ClientMessage, HandshakeRejection, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, ServerMessage,
};
use ginka_protocol::handshake::{DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, Handshake};

#[test]
fn a_hello_carries_the_version_and_the_token() {
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: "secret".into(),
    };
    let json = serde_json::to_string(&hello).unwrap();
    assert!(json.contains("\"type\":\"hello\""), "{json}");
    assert!(matches!(
        serde_json::from_str::<ClientMessage>(&json).unwrap(),
        ClientMessage::Hello { protocol_version, .. } if protocol_version == PROTOCOL_VERSION
    ));
}

#[test]
fn a_version_mismatch_is_refused_by_name_rather_than_half_working() {
    let rejected = ServerMessage::Rejected {
        reason: HandshakeRejection::VersionMismatch {
            daemon: PROTOCOL_VERSION + 1,
        },
    };
    let round_tripped: ServerMessage =
        serde_json::from_str(&serde_json::to_string(&rejected).unwrap()).unwrap();
    match round_tripped {
        ServerMessage::Rejected {
            reason: HandshakeRejection::VersionMismatch { daemon },
        } => assert_eq!(daemon, PROTOCOL_VERSION + 1),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_welcome_names_the_daemon_run() {
    let welcome = ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        version: "0.0.0".into(),
        epoch: 7,
        seq: 12,
    };
    let json = serde_json::to_string(&welcome).unwrap();
    assert!(json.contains("\"epoch\":7"), "{json}");
}

#[test]
fn the_wire_cap_leaves_room_for_an_attachment() {
    // Attachments travel over this socket, so the cap has to clear the largest
    // upload the daemon accepts (`ginka_core::attachment::MAX_ATTACHMENT_BYTES`)
    // with room for the envelope around it — otherwise a legal upload fails at
    // the transport, where the error means nothing to anyone.
    let largest_upload = 32 * 1024 * 1024_usize;
    assert!(MAX_WIRE_MESSAGE_BYTES > largest_upload);
}

#[test]
fn a_handshake_file_round_trips_and_addresses_loopback() {
    let handshake = Handshake {
        protocol_version: PROTOCOL_VERSION,
        port: 8123,
        token: "t".into(),
        pid: 42,
        version: "0.0.0".into(),
        epoch: 1_788_480_000,
    };
    assert_eq!(handshake.endpoint(), "ws://127.0.0.1:8123/rpc");

    let json = serde_json::to_string(&handshake).unwrap();
    assert_eq!(serde_json::from_str::<Handshake>(&json).unwrap(), handshake);
}

#[test]
fn a_handshake_from_a_newer_daemon_is_readable_enough_to_report_the_mismatch() {
    // The version has to survive a field this build has never heard of, or a
    // client cannot even tell the user *why* it cannot connect.
    let json = r#"{"protocol_version":99,"port":1,"token":"t","pid":2,"version":"9.9.9","epoch":3,"future":true}"#;
    let handshake: Handshake = serde_json::from_str(json).unwrap();
    assert_eq!(handshake.protocol_version, 99);
    assert!(!handshake.speaks_our_protocol());
}

#[test]
fn the_environment_overrides_are_named_once() {
    assert_eq!(DAEMON_ADDRESS_ENV, "GINKA_DAEMON_ADDRESS");
    assert_eq!(DAEMON_TOKEN_ENV, "GINKA_DAEMON_TOKEN");
}
