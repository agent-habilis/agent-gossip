#![expect(
    missing_docs,
    reason = "an integration test crate has no public docs; the module path names it"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use agent_gossip::Channel;
use agent_gossip_test_fixtures as common;
use common::{CONNECT_TIMEOUT, InProcNode, MSG_TIMEOUT, Node, POLL, RECOVERY_TIMEOUT};
use serde_json::{Value, json};

const SHARED: &str = "dup";

/// The slots of `doc` whose entry names `SHARED`, in slot order.
fn shared_slots(doc: &Value) -> Vec<String> {
    let Some(slots) = doc.as_object() else {
        return Vec::new();
    };
    let names_shared =
        |entry: &Value| entry.get("nickname").and_then(Value::as_str) == Some(SHARED);
    let entries = slots.iter().filter(|(_, entry)| names_shared(entry));
    let mut named: Vec<String> = entries.map(|(slot, _)| slot.clone()).collect();
    named.sort();
    named
}

fn is_key_hex(slot: &str) -> bool {
    let is_hex_digit = |byte: u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
    slot.len() == 64 && slot.bytes().all(is_hex_digit)
}

/// Wait until `node` holds `count` entries that name `SHARED`.
async fn await_shared_slots(node: &InProcNode, count: usize) -> Vec<String> {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let slots = shared_slots(&node.meta_get().await);
        if slots.len() == count {
            return slots;
        }
        assert!(
            Instant::now() < deadline,
            "{} never held {count} entries of '{SHARED}': {slots:?}",
            node.nickname
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Two live members use one nickname, and both write an entry that names it. A third member admits
/// one key for a nickname, so it holds the entry of that key only: the slot is the author key in hex
/// of the admitted member, and the entry of the refused key is not in its view.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_nickname_holds_one_slot_for_the_admitted_key() {
    let alice = InProcNode::create_with_nick("mbk-cards", SHARED).await;
    let beta = InProcNode::join(&alice.mesh, SHARED).await;
    let observer = InProcNode::join(&alice.mesh, "mbk-observer").await;
    alice.meta_merge(json!({ "nickname": SHARED })).await;
    beta.meta_merge(json!({ "nickname": SHARED })).await;

    let slots = await_shared_slots(&observer, 1).await;
    let admitted = alice.key_seen_by(&observer).await.to_string();
    // The barrier: a second write of each key, sent after the first write of the refused key. The
    // observer holds the second write of the admitted key once it has had its turn, and it knows
    // the admitted key by the roster only, so both members write.
    let marker = json!({ "nickname": SHARED, "marker": 1 });
    alice.meta_merge(marker.clone()).await;
    beta.meta_merge(marker).await;
    let marker_deadline = Instant::now() + MSG_TIMEOUT;
    while observer.meta_get().await[&admitted]["marker"] != 1 {
        assert!(
            Instant::now() < marker_deadline,
            "the second write of the admitted key never reached the observer"
        );
        tokio::time::sleep(POLL).await;
    }
    let later = shared_slots(&observer.meta_get().await);

    assert!(
        is_key_hex(&slots[0]),
        "the slot '{}' is not an author key in hex",
        slots[0]
    );
    assert_eq!(
        later, slots,
        "the entry of the refused key reached the observer"
    );
    assert_eq!(
        slots[0], admitted,
        "the slot is not the key that the observer admitted for the nickname"
    );

    alice.leave().await;
    beta.leave().await;
    observer.leave().await;
}

/// BEFORE (`a_send_by_a_shared_nickname_is_refused_and_then_reaches_the_remaining_key`, the key-keyed
/// roster of main, #47): a send by a nickname that two live keys use was refused as ambiguous, and after one
/// key left it reached the other. AFTER: the nickname gate admits one key for a nickname, so the observer's
/// roster names one member and the send is not ambiguous: it reaches exactly one of the two keys, the one
/// that the observer admitted, which opens it. The body is sealed to that key, so the other key cannot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_by_a_shared_nickname_reaches_exactly_the_key_that_the_observer_admitted() {
    let mut alice = InProcNode::create_with_nick("mbk-send", SHARED).await;
    let mut beta = InProcNode::join(&alice.mesh, SHARED).await;
    let observer = InProcNode::join(&alice.mesh, "mbk-sender").await;
    common::warm_up(&observer, &mut [&mut alice, &mut beta]).await;

    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    loop {
        let sent = observer.try_msg(SHARED, "to-the-one-holder").await;
        if sent.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a send to a nickname that has one admitted holder never went out: {sent:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    let open_deadline = Instant::now() + MSG_TIMEOUT;
    let (alice_opened, beta_opened) = loop {
        let alice_opened = alice
            .wait_body("to-the-one-holder", Duration::from_secs(1))
            .await;
        let beta_opened = beta
            .wait_body("to-the-one-holder", Duration::from_secs(1))
            .await;
        if alice_opened || beta_opened {
            break (alice_opened, beta_opened);
        }
        assert!(
            Instant::now() < open_deadline,
            "neither key opened the message of the observer"
        );
    };
    let late_alice = alice
        .wait_body("to-the-one-holder", Duration::from_secs(3))
        .await;
    let late_beta = beta
        .wait_body("to-the-one-holder", Duration::from_secs(3))
        .await;
    assert_eq!(
        [alice_opened || late_alice, beta_opened || late_beta]
            .iter()
            .filter(|opened| **opened)
            .count(),
        1,
        "the message must be sealed to one key, and only that key opens it"
    );

    alice.leave().await;
    beta.leave().await;
    observer.leave().await;
}

/// One GET against the local A2A binding; returns `(status, body)`.
fn get(port: u16, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the a2a port");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("the server stays up until the client has sent its request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read the response");
    let code: &str = raw
        .split_whitespace()
        .nth(1)
        .expect("a well-formed HTTP response starts with a status line");
    let status: u16 = code.parse().expect("the status code is a number");
    let (_, body) = raw.split_once("\r\n\r\n").unwrap_or_default();
    (status, body.to_owned())
}

/// The port of the local A2A binding, once the daemon is ready.
fn wait_port(state_file: &std::path::Path) -> u16 {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(raw) = std::fs::read_to_string(state_file)
            && let Ok(parsed) = serde_json::from_str::<Value>(&raw)
            && parsed["ready"] == true
            && let Some(port) = parsed["a2a_port"].as_u64()
        {
            return u16::try_from(port).expect("a port fits u16");
        }
        std::thread::sleep(POLL);
    }
    panic!("the daemon never reported a bound a2a port");
}

/// The card that the route serves for `path`, once it answers 200.
fn await_served_card(port: u16, path: &str) -> Value {
    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let (status, body) = get(port, path);
        if status == 200 {
            return serde_json::from_str(&body).expect("the route answers 200 with a JSON card");
        }
        assert!(
            Instant::now() < deadline,
            "the route never served {path}: {status} {body}"
        );
        std::thread::sleep(POLL);
    }
}

/// The author key of the daemon that runs on `state_file`, from its `ready` output.
fn ready_pubkey(state_file: &std::path::Path) -> String {
    let output = common::test_cmd()
        .args(["ready", "--state-file"])
        .arg(state_file)
        .output()
        .expect("the ready command spawns");
    assert!(
        output.status.success(),
        "the ready gate failed, so the key of the daemon is unknown: {output:?}"
    );
    let ready: Value =
        serde_json::from_slice(&output.stdout).expect("ready prints one JSON object");
    ready["pubkey"]
        .as_str()
        .expect("ready names the author key of the daemon")
        .to_owned()
}

/// The identity that a served card names: the author key in the first interface.
fn identity_of(card: &Value) -> Option<&str> {
    card.pointer("/supportedInterfaces/0/url")
        .and_then(Value::as_str)
}

/// The peer-card route takes a nickname or the key in hex. A host admits one key for a nickname, the
/// first that proved it, so the nickname gives the card of that key and stays with it when a second key
/// claims the same nickname. The key in hex gives the card of the admitted key. The refused key is no
/// member of the host, so its hex gets a 404: the daemon builds a card only from its roster.
#[test]
fn the_peer_card_route_serves_the_admitted_key_for_a_shared_nickname_and_its_hex() {
    let folder =
        std::env::temp_dir().join(format!("agent-gossip-meta-by-key-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("the temp folder is writable");
    let state_of = |name: &str| folder.join(format!("{name}.state.json"));
    let state_file = state_of("host");
    let state_path = state_file.to_string_lossy().to_string();
    let (host, mesh) = Node::create_args(
        "mbk-route",
        &["--a2a-serve", "--state-file", &state_path],
        &[],
    );
    // Two daemons of one mesh and one nickname with no state file of their own share one member key,
    // so they are one member and the newer run replaces the older. A state file for each makes two
    // members that claim one nickname.
    let join_as_member = |name: &str| {
        let state = state_of(name).to_string_lossy().to_string();
        Node::join_args(&mesh, SHARED, &["--state-file", &state], &[])
    };
    let first = join_as_member("first");
    assert!(host.wait_ready(&mesh), "the host socket never appeared");
    let port = wait_port(&state_file);
    let route = format!("/peers/{SHARED}/.well-known/agent-card.json");
    let first_key = ready_pubkey(&state_of("first"));

    // The host serves the card of the first key by the nickname, before a second key claims the
    // nickname: the host holds the proof of the first key when the second one comes.
    let served_first = await_served_card(port, &route);
    assert_eq!(
        identity_of(&served_first),
        Some(first_key.as_str()),
        "the host serves the first nickname holder with the card of another key"
    );
    let second = join_as_member("second");
    let second_key = ready_pubkey(&state_of("second"));
    assert_ne!(first_key, second_key, "the premise: two daemons, two keys");
    // No event marks a frame that the gate of the host refuses, so the test gives the second key
    // time to arrive and be refused before it asks for the card.
    std::thread::sleep(Duration::from_secs(5));

    let (status, body) = get(port, &route);
    assert_eq!(
        status, 200,
        "a nickname with one admitted holder is not served: {body}"
    );
    let served_now: Value =
        serde_json::from_str(&body).expect("the route answers 200 with a JSON card");
    assert_eq!(
        identity_of(&served_now),
        Some(first_key.as_str()),
        "the nickname moved to the key that the gate refused"
    );

    let (key_status, key_body) = get(
        port,
        &format!("/peers/{first_key}/.well-known/agent-card.json"),
    );
    assert_eq!(
        key_status, 200,
        "the card of the admitted key in hex is not served: {key_body}"
    );
    let card: Value = serde_json::from_str(&key_body).expect("a served card is JSON");
    assert_eq!(card["name"], SHARED, "the served card names the nickname");
    assert_eq!(
        identity_of(&card),
        Some(first_key.as_str()),
        "the card of a key in hex is the card of another key"
    );

    let (refused_status, refused_body) = get(
        port,
        &format!("/peers/{second_key}/.well-known/agent-card.json"),
    );
    assert_eq!(
        refused_status, 404,
        "the refused key is no member of the host, so it has no card: {refused_body}"
    );

    drop((first, second, host));
    let _ = std::fs::remove_dir_all(&folder);
}

/// The graph skills write the entry of a member by its nickname, inside the own slice of the
/// member: `{"peers":{"<nick>":{...}}}`. The slice is under the key of a live member, so `absent`
/// must not list it. An entry in `absent` is dropped by the graph tools, and the graph would vanish.
#[test]
fn a_graph_entry_inside_the_slice_of_a_live_member_is_not_absent() {
    let (host, mesh) = Node::create_named("mbk-graph");
    let member = Node::join(&mesh, "mbk-graph-m");
    common::await_peer_card_cli(&mesh, &host.nickname, &member.nickname);
    common::await_peer_card_cli(&mesh, &member.nickname, &host.nickname);

    common::cli_meta_report(&mesh, &member.nickname);
    let slot = common::await_meta_slot_cli(&mesh, &host.nickname, &member.nickname);
    let nick = member.nickname.clone();
    let merge = format!(r#"{{"peers":{{"{nick}":{{"instance":"g"}}}}}}"#);
    let mut command = common::test_cmd();
    command
        .args(["meta", "merge", "--gossip", &mesh])
        .args(["--nickname", &member.nickname])
        .args(["--merge", &merge]);
    let merged: std::process::Output = command.output().expect("failed to run meta merge");
    assert!(merged.status.success(), "meta merge failed: {merged:?}");

    let deadline = Instant::now() + MSG_TIMEOUT;
    loop {
        let raw = common::cli_channel_get(Channel::Meta, &mesh, &host.nickname);
        let view: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        if view["document"][&slot]["peers"][&nick]["instance"] == "g" {
            let absent = view["absent"].as_array().expect("meta get carries absent");
            assert!(
                !absent.iter().any(|entry| entry == slot.as_str()),
                "the slice of a live member is listed as absent: {view}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the host never saw the graph entry: {raw}"
        );
        std::thread::sleep(POLL);
    }
    drop((host, member));
}

/// The `.md` files under `dir`, found by walking the tree.
fn markdown_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).expect("the skills directory is readable");
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.expect("a directory entry is readable").path();
        if path.is_dir() {
            files.extend(markdown_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "md") {
            files.push(path);
        }
    }
    files
}

/// The index of every `peers[...]` in `text` that a nickname fills: `peers[$NICKNAME]`,
/// `peers["$NICKNAME"]`, `peers[$NICK]`, `peers[nickname]`, `peers[$PEER_NAME]`, `peers[$ALIAS]`,
/// and the like. A slot is the hex of an author key, so an index that names a nickname, a name, an
/// alias or a label is a join by name, unless it also names a key or a slot.
fn nickname_indexes(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (start, _) in text.match_indices("peers[") {
        let rest = &text[start + "peers[".len()..];
        let Some(end) = rest.find(']') else {
            continue;
        };
        let inner = &rest[..end];
        let bare = inner.trim_matches(|ch: char| !ch.is_alphanumeric());
        let lower = bare.to_lowercase();
        let says_key = ["key", "slot"].iter().any(|word| lower.contains(word));
        let says_name = ["nick", "name", "alias", "label"]
            .iter()
            .any(|word| lower.contains(word));
        if says_name && !says_key {
            found.push(format!("peers[{inner}]"));
        }
    }
    found
}

/// The indexer must catch each spelling of a join by nickname, and let a join by key pass.
#[test]
fn the_nickname_index_finder_catches_every_spelling() {
    let bad = [
        "document.peers[$NICKNAME]",
        "document.peers[\"$NICKNAME\"]",
        "document.peers[$NICK]",
        "document.peers[nickname]",
        "peers[<nickname>]",
        "peers[ ${NICKNAME} ]",
        "peers['name']",
        "document.peers[$PEER_NAME]",
        "document.peers[$ALIAS]",
        "document.peers[$NICKNAME_OF_PEER]",
    ];
    for line in bad {
        assert!(
            !nickname_indexes(line).is_empty(),
            "the finder missed a join by nickname: {line}"
        );
    }
    let good = [
        "document.peers[$PUBKEY]",
        "document.peers[<pubkey>]",
        "document.peers[<slot>].nickname",
        "document.peers[slot]",
        "document.peers[$PEER_KEY]",
        "document.peers[$SLOT]",
        "document.peers[$PUBKEY_OF_NAME]",
    ];
    for line in good {
        assert!(
            nickname_indexes(line).is_empty(),
            "the finder flagged a join by key: {line}"
        );
    }
}

/// A meta slot is the hex of an author key, and two members can share a nickname. A skill that
/// reads the entry of a member as `document.peers[<nickname>]` reads the wrong entry, or none.
#[test]
fn no_skill_joins_the_meta_document_by_nickname() {
    let skills = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    for file in markdown_files(&skills) {
        let text = std::fs::read_to_string(&file).expect("a skill file is UTF-8 text");
        let found = nickname_indexes(&text);
        assert!(
            found.is_empty(),
            "{} joins the meta document by nickname: {found:?}",
            file.display()
        );
    }
}

/// The text of a skill file with each `<!-- include path="..." -->` line replaced by the file it
/// names, relative to the including file.
fn expanded(file: &std::path::Path) -> String {
    let text = std::fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("{} is not readable text: {error}", file.display()));
    let directory = file.parent().expect("a skill file has a directory");
    let mut out = String::new();
    for line in text.lines() {
        let after_tag = line.trim().strip_prefix("<!-- include path=\"");
        let included = after_tag.and_then(|rest| rest.split('"').next());
        match included {
            Some(path) => out.push_str(&expanded(&directory.join(path))),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// A skill that reads `$PUBKEY` must hold it after a context clear. The ready gate sets it for the
/// skills that start a session. Every other skill gets it from the Reattach procedure, so that
/// procedure must restore it, and the skill must include that procedure.
#[test]
fn a_skill_that_uses_pubkey_can_recover_it_after_a_reattach() {
    let skills = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let reattach = expanded(&skills.join("shared/reattach.md"));
    let mut users = 0;
    for entry in std::fs::read_dir(&skills).expect("the skills directory is readable") {
        let skill = entry
            .expect("a directory entry is readable")
            .path()
            .join("SKILL.md");
        if !skill.is_file() {
            continue;
        }
        let text = expanded(&skill);
        let own_text: String = text.split(reattach.as_str()).collect();
        if !own_text.contains("$PUBKEY") {
            continue;
        }
        users += 1;
        let sets_from_ready = text.contains("`$PUBKEY` = `pubkey`");
        let includes_reattach = text.contains(&reattach);
        assert!(
            sets_from_ready || includes_reattach,
            "{} uses $PUBKEY and neither the ready gate nor the Reattach procedure sets it",
            skill.display()
        );
    }
    assert!(users > 0, "no skill uses $PUBKEY, so the check is vacuous");
    assert!(
        reattach.contains("`$PUBKEY`"),
        "the Reattach procedure does not restore $PUBKEY"
    );
}
