use std::time::{Duration, Instant};

use crate::{MSG_TIMEOUT, cli_message_raw, cli_poll};

/// Block until a chat probe of `sender` reaches `receiver`.
///
/// The probe is sent again until the poll of the receiver holds it. A gossip holds no log of a
/// broadcast, so a probe that leaves before the link exists is lost, and nothing sends it again. A
/// merge of the state or the meta channel needs the same link, so a test calls this first.
///
/// The polls of the receiver advance its cursor past the probes.
///
/// # Panics
///
/// Panics if no probe reaches `receiver` before `MSG_TIMEOUT`.
pub fn await_link_cli(mesh: &str, sender: &str, receiver: &str) {
    let deadline = Instant::now() + MSG_TIMEOUT;
    for round in 0.. {
        let body = format!("link-probe-{round}");
        let _ = cli_message_raw(mesh, sender, &body);
        let round_end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < round_end {
            if cli_poll(mesh, receiver, None).contains(&body) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(
            Instant::now() < deadline,
            "no link probe of {sender} reached {receiver}"
        );
    }
}
