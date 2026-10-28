use super::{Output, PingPeer, ReadyParams, StateChangedParams};

/// The app renders the engine's generic
/// [`NodeEvent`](habilis_network::embed::NodeEvent)s by mapping each onto the
/// existing `Output` method, so the stdout JSON and tap forms stay
/// byte-identical. This seam lets the engine emit surfacings without naming the
/// concrete `Output`.
impl habilis_network::embed::NodeSink for Output {
    fn emit(&self, event: habilis_network::embed::NodeEvent) {
        use habilis_network::embed::NodeEvent;
        match event {
            // Bare identity: startup diagnostics (skill drift, the `--a2a-serve`
            // port) are ours, not the engine's, and are spliced in by
            // `StartupSink` for the paths that have them. Reached directly only
            // by a session with none.
            NodeEvent::Ready {
                mesh,
                name,
                nickname,
            } => self.ready(ReadyParams {
                mesh: &mesh,
                name: &name,
                nickname: &nickname,
                drift: None,
                a2a_port: None,
            }),
            NodeEvent::Info(message) => self.info(&message),
            NodeEvent::Error(message) => self.error(&message),
            NodeEvent::Fork {
                nickname,
                pubkey,
                seq,
            } => self.fork(&nickname, &pubkey, seq),
            NodeEvent::PeerTimeout {
                nickname,
                last_seen_secs_ago,
            } => self.peer_timeout(&nickname, last_seen_secs_ago),
            NodeEvent::PeerReturn { nickname } => self.peer_return(&nickname),
            NodeEvent::Presence { msg, .. } => self.print_presence(&msg),
            NodeEvent::PingReport { peers, known } => self.ping_report(
                peers
                    .into_iter()
                    .map(|(nickname, rtt_ms)| PingPeer { nickname, rtt_ms })
                    .collect(),
                known,
            ),
            NodeEvent::StateChanged {
                channel,
                event,
                document,
                is_self,
            } => self.state_changed(StateChangedParams {
                channel,
                event: &event,
                document: &document,
                is_self,
            }),
            #[expect(
                clippy::wildcard_enum_match_arm,
                reason = "foreign non_exhaustive enum: the compiler needs a wildcard arm; the named variants are listed before it"
            )]
            _ => {}
        }
    }
}
