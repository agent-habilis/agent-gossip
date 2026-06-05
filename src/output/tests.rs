//! Output-module tests: Human-mode coloring and the JSON wire format
//! (shape assertions + insta snapshots + proptests). The wire
//! serializers under test live in [`super::json`].

use super::json::{SimpleEvent, format_msg_json, format_presence_json};
use super::{Output, OutputMode, style};
use crate::protocol::{Message, PresenceSubtype};

fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("invalid JSON: {error}\n{text}"))
}

// ── Human-mode coloring (style::*) ─────────────────────────

fn human(self_nick: &str) -> Output {
    Output::new(OutputMode::Human, false, Some(self_nick.to_owned()))
}

#[test]
fn nick_ansi_self_vs_peer() {
    let out = human("alice");
    assert_eq!(
        out.nick_ansi("alice", true),
        (style::SELF_NICK, style::RESET)
    );
    assert_eq!(out.nick_ansi("bob", true), (style::PEER_NICK, style::RESET));
}

#[test]
fn nick_ansi_disabled_is_empty() {
    let out = human("alice");
    assert_eq!(out.nick_ansi("alice", false), ("", ""));
    assert_eq!(out.nick_ansi("bob", false), ("", ""));
}

#[test]
fn highlight_colors_swarm_and_self_and_peer() {
    let out = human("alice");
    let colored = out.highlight("created #team and joined as <alice>", true);
    assert_eq!(
        colored,
        format!(
            "created {}#team{} and joined as {}<alice>{}",
            style::SWARM,
            style::RESET,
            style::SELF_NICK,
            style::RESET
        )
    );
    // A peer nick in an info line gets the peer color.
    let peer = out.highlight("joined as <bob>", true);
    assert!(peer.contains(&format!("{}<bob>{}", style::PEER_NICK, style::RESET)));
}

#[test]
fn highlight_disabled_is_identity() {
    let out = human("alice");
    let text = "created #team and joined as <alice>";
    assert_eq!(out.highlight(text, false), text);
}

// ── format_msg_json: msg type ──────────────────────────────

fn nick(name: &str) -> crate::protocol::Nickname {
    crate::protocol::Nickname::from(name)
}

fn body(text: &str) -> crate::protocol::MessageBody {
    crate::protocol::MessageBody::from(text)
}

fn sid() -> crate::protocol::SwarmId {
    crate::protocol::SwarmId::from("ahstest")
}

#[test]
fn json_message_has_all_fields() {
    let msg = Message::new_message(&sid(), &nick("alice"), body("hello"));
    let json = format_msg_json(&msg, false);
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "message");
    assert_eq!(parsed["type"], "msg");
    assert_eq!(parsed["swarm"], "ahstest");
    assert_eq!(parsed["author"], "alice");
    assert_eq!(parsed["body"], "hello");
    assert!(parsed["reply"].is_null());
    assert_eq!(parsed["self"], false);
    assert!(parsed["id"].is_string());
    assert!(parsed["ts"].is_number());
    // Unsigned (test fixture): the pubkey field is omitted.
    assert!(parsed.get("pubkey").is_none());
}

#[test]
fn json_signed_message_exposes_full_pubkey() {
    // A real (signed) message carries the author's full Ed25519 public key
    // (hex) in the JSON event — the identity behind the display nickname.
    let identity = crate::protocol::identity::Identity::generate();
    let msg = Message::new_message(&sid(), &nick("alice"), body("hi")).signed(&identity);
    let parsed = parse(&format_msg_json(&msg, false));
    let pubkey = parsed["pubkey"].as_str().expect("signed event has pubkey");
    assert_eq!(pubkey, msg.pubkey, "event pubkey must be the message's key");
    assert_eq!(pubkey.len(), 64, "full 32-byte key as hex");
}

#[test]
fn fork_event_json_shape() {
    use super::OutputEvent;
    let pubkey = "deadbeef".repeat(8); // 64 hex
    let json = super::json::event_json(&OutputEvent::Fork {
        nickname: nick("alice"),
        pubkey: pubkey.clone(),
        seq: 7,
    })
    .expect("fork event produces a JSON line");
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "fork");
    assert_eq!(parsed["nickname"], "alice");
    assert_eq!(parsed["pubkey"], pubkey);
    assert_eq!(parsed["seq"], 7);
}

#[test]
fn json_reply_has_target_nickname() {
    let reply = Message::new_reply(&sid(), &nick("bob"), nick("alice"), body("a"));
    let parsed = parse(&format_msg_json(&reply, false));
    assert_eq!(parsed["reply"].as_str().unwrap(), "alice");
}

#[test]
fn json_self_flag_true() {
    let msg = Message::new_message(&sid(), &nick("alice"), body("hi"));
    let parsed = parse(&format_msg_json(&msg, true));
    assert_eq!(parsed["self"], true);
}

#[test]
fn json_body_escapes_quotes() {
    let msg = Message::new_message(&sid(), &nick("alice"), body(r#"say "hello""#));
    let json = format_msg_json(&msg, false);
    let parsed = parse(&json);
    assert_eq!(parsed["body"], r#"say "hello""#);
}

#[test]
fn json_body_escapes_backslashes() {
    let msg = Message::new_message(&sid(), &nick("alice"), body(r"path\to\file"));
    let json = format_msg_json(&msg, false);
    let parsed = parse(&json);
    assert_eq!(parsed["body"], r"path\to\file");
}

// ── format_msg_json: presence type ─────────────────────────

#[test]
fn json_presence_joined() {
    let msg = Message::new_joined(&sid(), &nick("alice"));
    let parsed = parse(&format_presence_json(&msg, PresenceSubtype::Joined));
    assert_eq!(parsed["event"], "message");
    assert_eq!(parsed["type"], "presence");
    assert_eq!(parsed["subtype"], "joined");
    assert_eq!(parsed["author"], "alice");
    assert_eq!(parsed["swarm"], "ahstest");
}

#[test]
fn json_presence_left() {
    let msg = Message::new_left(&sid(), &nick("bob"));
    let parsed = parse(&format_presence_json(&msg, PresenceSubtype::Left));
    assert_eq!(parsed["subtype"], "left");
    assert_eq!(parsed["author"], "bob");
}

// ── peer_timeout / peer_return shape ───────────────────────

#[test]
fn peer_timeout_json_shape() {
    let json = serde_json::to_string(&SimpleEvent::PeerTimeout {
        nickname: "ball-blue",
        last_seen_secs_ago: 94,
        display: "🐝️ `<ball-blue>` went quiet".to_owned(),
    })
    .unwrap();
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "peer_timeout");
    assert_eq!(parsed["nickname"], "ball-blue");
    assert_eq!(parsed["last_seen_secs_ago"], 94);
    assert_eq!(parsed["display"], "🐝️ `<ball-blue>` went quiet");
}

#[test]
fn peer_return_json_shape() {
    let json = serde_json::to_string(&SimpleEvent::PeerReturn {
        nickname: "ball-blue",
        display: "🐝️ `<ball-blue>` came back".to_owned(),
    })
    .unwrap();
    let parsed = parse(&json);
    assert_eq!(parsed["event"], "peer_return");
    assert_eq!(parsed["nickname"], "ball-blue");
    assert_eq!(parsed["display"], "🐝️ `<ball-blue>` came back");
}

// ── all outputs are valid single-line JSON ─────────────────────

#[test]
fn json_output_is_single_line() {
    let msgs: Vec<String> = vec![
        format_msg_json(
            &Message::new_message(&sid(), &nick("alice"), body("hello world")),
            false,
        ),
        format_msg_json(
            &Message::new_reply(&sid(), &nick("bob"), nick("alice"), body("reply")),
            false,
        ),
        format_presence_json(
            &Message::new_joined(&sid(), &nick("charlie")),
            PresenceSubtype::Joined,
        ),
        format_presence_json(
            &Message::new_left(&sid(), &nick("dave")),
            PresenceSubtype::Left,
        ),
    ];
    for json in msgs {
        assert!(!json.contains('\n'), "JSON should be single line: {json}");
        assert!(parse(&json).is_object());
    }
}

mod snapshots {
    use super::{format_msg_json, format_presence_json};
    use crate::protocol::{Message, MessageKind, PresenceSubtype};

    #[test]
    fn snap_msg_message() {
        let msg = Message::fixture(MessageKind::Msg { reply: None }, "What is Rust?");
        insta::assert_snapshot!(format_msg_json(&msg, false));
    }

    #[test]
    fn snap_msg_reply() {
        let msg = Message::fixture(
            MessageKind::Msg {
                reply: Some(crate::protocol::Nickname::from("addressed-nick")),
            },
            "Rust is a systems language.",
        );
        insta::assert_snapshot!(format_msg_json(&msg, false));
    }

    #[test]
    fn snap_presence_joined() {
        let msg = Message::fixture(
            MessageKind::Presence {
                subtype: PresenceSubtype::Joined,
            },
            "",
        );
        insta::assert_snapshot!(format_presence_json(&msg, PresenceSubtype::Joined));
    }

    #[test]
    fn snap_presence_left() {
        let msg = Message::fixture(
            MessageKind::Presence {
                subtype: PresenceSubtype::Left,
            },
            "",
        );
        insta::assert_snapshot!(format_presence_json(&msg, PresenceSubtype::Left));
    }
}

mod prop {
    use proptest::{
        collection::vec as arb_vec, prelude::any, prop_assert, prop_assert_eq, proptest,
        strategy::Strategy,
    };

    use super::{PresenceSubtype, format_msg_json, format_presence_json, sid};
    use crate::protocol::Message;

    fn arb_ascii_body() -> impl Strategy<Value = String> {
        arb_vec(0x20u8..0x7Eu8, 0..200).prop_map(|bytes| String::from_utf8(bytes).unwrap())
    }

    fn arb_nickname() -> impl Strategy<Value = crate::protocol::Nickname> {
        "[a-z]{3,8}-[a-z]{3,8}".prop_map(|raw| crate::protocol::Nickname::new(raw).unwrap())
    }

    proptest! {
        #![proptest_config(crate::proptest_support::config())]
        #[test]
        fn prop_message_json_is_valid(
            body_text in arb_ascii_body(),
            author in arb_nickname(),
            is_self in any::<bool>(),
        ) {
            let expected = body_text.clone();
            let msg = Message::new_message(
                &sid(),
                &author,
                crate::protocol::MessageBody::new(body_text).unwrap(),
            );
            let json = format_msg_json(&msg, is_self);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(parsed["body"].as_str().unwrap(), expected.as_str());
            prop_assert!(!json.contains('\n'));
        }

        #[test]
        fn prop_reply_json_is_valid(
            body_text in arb_ascii_body(),
            author in arb_nickname(),
            target in arb_nickname(),
        ) {
            let expected_target = target.as_str().to_string();
            let expected_body = body_text.clone();
            let msg = Message::new_reply(
                &sid(),
                &author,
                target,
                crate::protocol::MessageBody::new(body_text).unwrap(),
            );
            let json = format_msg_json(&msg, false);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(parsed["reply"].as_str().unwrap(), expected_target.as_str());
            prop_assert_eq!(parsed["body"].as_str().unwrap(), expected_body.as_str());
        }

        #[test]
        fn prop_presence_json_is_valid(is_join in any::<bool>()) {
            let test_nick = crate::protocol::Nickname::from("test-nick");
            let (msg, subtype) = if is_join {
                (Message::new_joined(&sid(), &test_nick), PresenceSubtype::Joined)
            } else {
                (Message::new_left(&sid(), &test_nick), PresenceSubtype::Left)
            };
            let json = format_presence_json(&msg, subtype);
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&parsed["type"], "presence");
        }
    }
}
