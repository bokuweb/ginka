//! Who gets to talk to the daemon, and what they are told when they do not.

use ginka_core::daemon;
use ginka_protocol::envelope::{
    ClientMessage, HandshakeRejection, PROTOCOL_VERSION, ServerMessage,
};

const EPOCH: u64 = 1_788_480_000;
const SEQ: u64 = 12;
const VERSION: &str = "0.0.0";
const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn hello(version: u32, token: &str) -> ClientMessage {
    ClientMessage::Hello {
        protocol_version: version,
        token: token.into(),
    }
}

#[test]
fn a_matching_version_and_token_is_welcomed_into_this_run() {
    let answer = daemon::greet(TOKEN, EPOCH, SEQ, VERSION, &hello(PROTOCOL_VERSION, TOKEN));
    assert!(matches!(
        answer,
        ServerMessage::Welcome {
            protocol_version,
            epoch,
            seq,
            ..
        } if protocol_version == PROTOCOL_VERSION && epoch == EPOCH && seq == SEQ
    ));
}

#[test]
fn a_wrong_token_is_refused_without_explaining_itself() {
    let answer = daemon::greet(
        TOKEN,
        EPOCH,
        SEQ,
        VERSION,
        &hello(PROTOCOL_VERSION, "not-the-token"),
    );
    assert!(matches!(
        answer,
        ServerMessage::Rejected {
            reason: HandshakeRejection::BadToken
        }
    ));
}

#[test]
fn the_version_is_checked_before_the_token() {
    // A client on the wrong contract should be told *that*, not sent chasing a
    // token problem it does not have.
    let answer = daemon::greet(
        TOKEN,
        EPOCH,
        SEQ,
        VERSION,
        &hello(PROTOCOL_VERSION + 1, "wrong too"),
    );
    assert!(matches!(
        answer,
        ServerMessage::Rejected {
            reason: HandshakeRejection::VersionMismatch { daemon }
        } if daemon == PROTOCOL_VERSION
    ));
}

#[test]
fn nothing_is_answered_before_the_hello() {
    let answer = daemon::greet(
        TOKEN,
        EPOCH,
        SEQ,
        VERSION,
        &ClientMessage::Request {
            id: 1,
            payload: ginka_protocol::rpc::Request::ListProjects,
        },
    );
    assert!(matches!(
        answer,
        ServerMessage::Rejected {
            reason: HandshakeRejection::HelloExpected
        }
    ));
}

#[test]
fn a_resume_before_the_hello_is_refused_too() {
    let answer = daemon::greet(
        TOKEN,
        EPOCH,
        SEQ,
        VERSION,
        &ClientMessage::Resume { after: 4 },
    );
    assert!(matches!(
        answer,
        ServerMessage::Rejected {
            reason: HandshakeRejection::HelloExpected
        }
    ));
}

#[test]
fn an_accepted_handshake_is_recognisable_without_matching_the_message() {
    assert!(
        daemon::greet(TOKEN, EPOCH, SEQ, VERSION, &hello(PROTOCOL_VERSION, TOKEN)).is_welcome()
    );
    assert!(
        !daemon::greet(TOKEN, EPOCH, SEQ, VERSION, &hello(PROTOCOL_VERSION, "no")).is_welcome()
    );
}
