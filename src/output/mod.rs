use std::io::IsTerminal;
use std::sync::LazyLock;

use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::swarm::SwarmName;
use crate::protocol::{Message, MessageId, MessageKind, Nickname, SwarmId, TaskId};

mod json;
#[cfg(test)]
mod tests;

pub(crate) use json::PingPeer;
use json::{
    SimpleEvent, emit, emit_json, format_presence_json, peer_return_display, peer_timeout_display,
    ping_report_display, print_message_json,
};
pub use json::{event_json, surfaced_event_json};

/// Output mode — chosen per event loop at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Human,
    Json,
    /// Silent mode — suppresses all stdout and stderr user-facing
    /// output. Used by the MCP server, where stdout is reserved for
    /// the JSON-RPC protocol transport and any print would corrupt
    /// the stream.
    Silent,
}

/// Owned, structured form of every line the daemon would print.
/// In-process consumers ([`crate::embed::SwarmSession::events`],
/// tests) receive these over a channel instead of scraping stdout;
/// the `Stream` sink renders the *same* data through `events.rs`, so
/// the wire format stays byte-identical.
///
/// `#[non_exhaustive]`: a future event kind must not be a breaking
/// change for embedders matching on this.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum OutputEvent {
    Ready {
        swarm: SwarmId,
        name: SwarmName,
        nickname: Nickname,
        /// One-line skill-drift warning when the installed integration has
        /// fallen behind this binary (see `cli::agent::drift_warning`), else
        /// `None`. The startup nag for stale skills.
        drift: Option<String>,
    },
    SwarmId {
        id: SwarmId,
    },
    Message {
        msg: Box<Message>,
        is_self: bool,
    },
    Presence {
        msg: Box<Message>,
    },
    PeerTimeout {
        nickname: Nickname,
        last_seen_secs_ago: u64,
    },
    PeerReturn {
        nickname: Nickname,
    },
    /// Equivocation detected (Phase 2): an author key signed two different
    /// messages at the same `seq`. Surfaced once per offending key.
    Fork {
        nickname: Nickname,
        pubkey: String,
        seq: u64,
    },
    MsgPosted {
        id: MessageId,
    },
    Info {
        message: String,
    },
    Error {
        message: String,
    },
    PingReport {
        peers: Vec<PingPeer>,
        known: usize,
    },
    /// One leg of a task surfaced to the addressee (or the
    /// sender's own echo). Carries the full `Message` so the JSON sink can
    /// render `to`/`task_id`/`phase`/`body`. The JSON sink renders
    /// the `Progress` phase as a `task_progress` event and every other
    /// phase as a `task` event.
    Task {
        msg: Box<Message>,
        is_self: bool,
    },
    /// A task was evicted for crossing its idle-debounce timeout (the
    /// daemon-side per-task silence sweep). Daemon-originated — no `Message`.
    TaskTimeout {
        task_id: TaskId,
    },
    /// A shared-state change: the patch event, the freshly-derived document, and
    /// whether it was our own write. Drives the reaction hook and the human
    /// `🐝 … changed shared state` line.
    StateChanged {
        channel: crate::protocol::Channel,
        event: Box<Message>,
        document: serde_json::Value,
        is_self: bool,
    },
}

/// ANSI styling for the Human-mode `<nick>` / `#swarm` tokens.
/// Hand-rolled (no color dependency); emission is gated on a TTY +
/// `NO_COLOR` per stream (see [`stdout_color`] / [`stderr_color`]).
pub(crate) mod style {
    pub(crate) const RESET: &str = "\x1b[0m";
    /// Bold green — the local member's own nickname.
    pub(super) const SELF_NICK: &str = "\x1b[1;32m";
    /// Bold cyan — a peer's nickname.
    pub(super) const PEER_NICK: &str = "\x1b[1;36m";
    /// Bold yellow — a swarm name.
    pub(crate) const SWARM: &str = "\x1b[1;33m";
    /// Bold blue — the runnable hint (`ahsw join` on create, `ahsw file get`
    /// on a file producer).
    pub(crate) const BLUE: &str = "\x1b[1;34m";
    /// Bold — the highlighted row in the `discover` picker.
    pub(crate) const BOLD: &str = "\x1b[1m";
}

/// Color is on when the stream is a terminal and `NO_COLOR` is unset.
fn color_enabled(stream_is_terminal: bool) -> bool {
    stream_is_terminal && std::env::var_os("NO_COLOR").is_none()
}

/// Whether stdout should carry ANSI color. Cached — neither the TTY
/// status nor the env var changes at runtime, and piped/non-TTY output
/// (tests, the `/swarm` skill) auto-disables.
pub(crate) fn stdout_color() -> bool {
    static ENABLED: LazyLock<bool> =
        LazyLock::new(|| color_enabled(std::io::stdout().is_terminal()));
    *ENABLED
}

/// Stderr counterpart to [`stdout_color`] (presence/info/timeout lines).
fn stderr_color() -> bool {
    static ENABLED: LazyLock<bool> =
        LazyLock::new(|| color_enabled(std::io::stderr().is_terminal()));
    *ENABLED
}

/// The ANSI `(prefix, suffix)` wrapping a `#swarm` token — empty when
/// disabled, so callers interpolate inline with no allocation either way.
fn swarm_ansi(enabled: bool) -> (&'static str, &'static str) {
    if enabled {
        (style::SWARM, style::RESET)
    } else {
        ("", "")
    }
}

/// Per-event-loop output destination. Replaces the former
/// process-global `MODE`/`FILTER_SELF` statics so multiple in-process
/// sessions (embed facade, tests) can run with independent output
/// instead of racing a first-write-wins `OnceLock`. Threaded by
/// `&Output` (not `Copy` — `Capture` holds a channel sender).
#[derive(Debug, Clone)]
pub(crate) enum Output {
    /// Write to the process stdout/stderr in the given mode (CLI /
    /// MCP-silent). `self_nick` is the local member's nickname, used to
    /// paint your own `<nick>` distinctly from peers' in Human mode.
    ///
    /// `tap` is an optional fan-out: when set, every surfaced
    /// [`OutputEvent`] is *also* sent here, in addition to being rendered
    /// to stdout. The CLI daemon attaches one so the event loop can mirror
    /// the surfaced stream into its local `surfaced_events` ring (the
    /// `poll`/`fetch` history), without each emit site needing `&mut
    /// state`. The tap carries the *same* structured events the `Capture`
    /// sink would, so a tapped event renders byte-identically through
    /// [`event_json`] later. Self-filtering still applies before the tap
    /// (a suppressed self-message is neither printed nor tapped).
    Stream {
        mode: OutputMode,
        filter_self: bool,
        self_nick: Option<String>,
        tap: Option<UnboundedSender<OutputEvent>>,
    },
    /// Push structured events into an in-process channel (embed
    /// facade / tests). Mode-agnostic: every event is captured.
    ///
    /// `tap` mirrors each event into the daemon's `surfaced_events` ring,
    /// exactly as the `Stream` tap does — so the embed facade's `fetch`
    /// (pull) sees the same events its `events()` subscription (push) does.
    Capture {
        tx: UnboundedSender<OutputEvent>,
        filter_self: bool,
        tap: Option<UnboundedSender<OutputEvent>>,
    },
}

impl Output {
    pub(crate) fn new(mode: OutputMode, filter_self: bool, self_nick: Option<String>) -> Self {
        Self::Stream {
            mode,
            filter_self,
            self_nick,
            tap: None,
        }
    }

    /// Attach a fan-out `tap`, returning the tapped sink. Every surfaced
    /// [`OutputEvent`] is then *also* sent to `tap` (the daemon's
    /// `surfaced_events` mirror) in addition to being rendered/captured.
    /// Applies to BOTH the `Stream` sink (CLI daemon) and the `Capture` sink
    /// (embed/MCP — so its `fetch` sees the same events its `events()`
    /// subscription does); replaces any existing tap (there is only ever one
    /// attach site). Lets the event loop wrap the sink it was handed without
    /// `EventLoopConfig` carrying a receiver.
    pub(crate) fn with_tap(self, tap: UnboundedSender<OutputEvent>) -> Self {
        match self {
            Output::Stream {
                mode,
                filter_self,
                self_nick,
                tap: _,
            } => Output::Stream {
                mode,
                filter_self,
                self_nick,
                tap: Some(tap),
            },
            Output::Capture {
                tx, filter_self, ..
            } => Output::Capture {
                tx,
                filter_self,
                tap: Some(tap),
            },
        }
    }

    /// Suppresses all stdout/stderr output (MCP server).
    pub(crate) fn silent() -> Self {
        Self::Stream {
            mode: OutputMode::Silent,
            filter_self: false,
            self_nick: None,
            tap: None,
        }
    }

    /// The local member's nickname, when known (Human-mode self-color).
    fn self_nick(&self) -> Option<&str> {
        match self {
            Output::Stream { self_nick, .. } => self_nick.as_deref(),
            Output::Capture { .. } => None,
        }
    }

    /// The ANSI `(prefix, suffix)` wrapping a `<name>` token —
    /// [`style::SELF_NICK`] for the local member, else
    /// [`style::PEER_NICK`]; empty when disabled. Callers interpolate
    /// inline so a disabled line allocates nothing extra.
    fn nick_ansi(&self, name: &str, enabled: bool) -> (&'static str, &'static str) {
        if !enabled {
            return ("", "");
        }
        let color = if self.self_nick() == Some(name) {
            style::SELF_NICK
        } else {
            style::PEER_NICK
        };
        (color, style::RESET)
    }

    /// Colorize `<nick>` / `#swarm` tokens in a pre-composed `info`
    /// string. Safe because nicknames/swarm names exclude `< > #` and
    /// whitespace, so the markers are unambiguous delimiters and the
    /// string never carries free-form message-body text.
    fn highlight(&self, text: &str, enabled: bool) -> String {
        if !enabled {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '<' => {
                    let mut name = String::new();
                    let mut closed = false;
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next == '>' {
                            closed = true;
                            break;
                        }
                        name.push(next);
                    }
                    if closed {
                        let (open, close) = self.nick_ansi(&name, true);
                        out.push_str(open);
                        out.push('<');
                        out.push_str(&name);
                        out.push('>');
                        out.push_str(close);
                    } else {
                        out.push('<');
                        out.push_str(&name);
                    }
                }
                '#' => {
                    let mut name = String::new();
                    while let Some(&next) = chars.peek() {
                        if next.is_whitespace() || matches!(next, '<' | '>' | '#') {
                            break;
                        }
                        name.push(next);
                        chars.next();
                    }
                    if name.is_empty() {
                        out.push('#');
                    } else {
                        let (open, close) = swarm_ansi(true);
                        out.push_str(open);
                        out.push('#');
                        out.push_str(&name);
                        out.push_str(close);
                    }
                }
                other => out.push(other),
            }
        }
        out
    }

    /// Capture every event into `tx` (embed facade / in-process
    /// tests).
    pub(crate) fn capture(tx: UnboundedSender<OutputEvent>) -> Self {
        Self::Capture {
            tx,
            filter_self: false,
            tap: None,
        }
    }

    fn filters_self(&self) -> bool {
        match self {
            Self::Stream { filter_self, .. } | Self::Capture { filter_self, .. } => *filter_self,
        }
    }

    /// The one sink skeleton every event method shares: in `Capture`
    /// mode push the structured [`OutputEvent`]; in `Stream` mode render
    /// that event's own human / JSON / (silent) form, and — when a `tap`
    /// is attached — *also* push the structured event so the daemon's
    /// `surfaced_events` ring sees the identical stream. Factoring this
    /// out is what removes the ~10× duplicated `match Stream/Capture`.
    fn dispatch(&self, event: impl FnOnce() -> OutputEvent, stream: impl FnOnce(OutputMode)) {
        match self {
            Output::Stream { mode, tap, .. } => {
                stream(*mode);
                if let Some(tap) = tap {
                    let _ = tap.send(event());
                }
            }
            Output::Capture { tx, tap, .. } => {
                // Build once; the live `events()` subscription and the
                // `surfaced_events` ring (tap) each get a copy.
                let event = event();
                if let Some(tap) = tap {
                    let _ = tap.send(event.clone());
                }
                let _ = tx.send(event);
            }
        }
    }

    pub(crate) fn ready(
        &self,
        swarm: &SwarmId,
        name: &SwarmName,
        nickname: &Nickname,
        drift: Option<&str>,
    ) {
        self.dispatch(
            || OutputEvent::Ready {
                swarm: swarm.clone(),
                name: name.clone(),
                nickname: nickname.clone(),
                drift: drift.map(str::to_owned),
            },
            |mode| {
                // In human mode, `info("joined as <nick>")` covers this.
                if mode == OutputMode::Json {
                    emit_json(&SimpleEvent::Ready {
                        version: crate::VERSION,
                        swarm: swarm.as_str(),
                        name: name.as_str(),
                        nickname: nickname.as_str(),
                        drift,
                    });
                }
            },
        );
    }

    /// Surface the swarm identifier at startup (stderr). Human mode
    /// prints the runnable join command (`ahsw join <id>`); JSON mode
    /// prints the canonical `🐝://…` id (the integration harness greps this);
    /// Silent suppresses it.
    pub(crate) fn swarm_id_line(&self, id: &SwarmId) {
        self.dispatch(
            || OutputEvent::SwarmId { id: id.clone() },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = if stderr_color() {
                        (style::BLUE, style::RESET)
                    } else {
                        ("", "")
                    };
                    eprintln!("others can join with: {open}ahsw join {id}{close}");
                }
                OutputMode::Json => eprintln!("{id}"),
                OutputMode::Silent => {}
            },
        );
    }

    pub(crate) fn print_message(&self, msg: &Message) {
        self.print_message_ex(msg, false);
    }

    /// Print a message with an optional self flag (for echo-back).
    /// When `filter_self` is enabled, self-authored messages are
    /// suppressed. Presence events should use `print_presence` instead.
    pub(crate) fn print_message_ex(&self, msg: &Message, is_self: bool) {
        if matches!(
            msg.kind,
            MessageKind::PeerInfo
                | MessageKind::Digest
                | MessageKind::StateDigest
                | MessageKind::MetaDigest
                | MessageKind::Ping
                | MessageKind::Pong { .. }
        ) {
            return;
        }
        if is_self && self.filters_self() {
            return;
        }
        self.dispatch(
            || OutputEvent::Message {
                msg: Box::new(msg.clone()),
                is_self,
            },
            |mode| match mode {
                OutputMode::Human => self.print_message_human(msg),
                OutputMode::Json => print_message_json(msg, is_self),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface a task leg. Distinct top-level `task` event (not
    /// the `message` family) so skills branch on `event`. When
    /// `filter_self` is enabled, self-authored legs are suppressed.
    pub(crate) fn print_task(&self, msg: &Message, is_self: bool) {
        let MessageKind::Task { to, phase, .. } = &msg.kind else {
            return;
        };
        if is_self && self.filters_self() {
            return;
        }
        let (to, phase) = (to.clone(), *phase);
        self.dispatch(
            || OutputEvent::Task {
                msg: Box::new(msg.clone()),
                is_self,
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(msg.author.as_str(), stdout_color());
                    let (to_open, to_close) = self.nick_ansi(to.as_str(), stdout_color());
                    println!(
                        "task {phase} {open}<{}>{close} → {to_open}<{to}>{to_close}: {}",
                        msg.author, msg.body
                    );
                }
                OutputMode::Json => json::print_task_json(msg, is_self),
                OutputMode::Silent => {}
            },
        );
    }

    /// A task crossed its idle-debounce timeout and was evicted by the
    /// daemon's per-task silence sweep (the task analogue of
    /// [`peer_timeout`](Self::peer_timeout)). Human mode prints a stderr
    /// note; JSON mode emits a `task_timeout` event for the widget.
    pub(crate) fn task_timeout(&self, task_id: &TaskId) {
        self.dispatch(
            || OutputEvent::TaskTimeout {
                task_id: task_id.clone(),
            },
            |mode| match mode {
                OutputMode::Human => {
                    eprintln!("task {task_id} timed out");
                }
                OutputMode::Json => emit_json(&SimpleEvent::TaskTimeout {
                    task_id: task_id.as_str(),
                }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface a shared-state change: the human `🐝 … changed shared state`
    /// line, the structured `state` event for the agent API (poll / `--output
    /// json` / embed `events()`), and the surfaced-ring mirror. F5: a *self*
    /// change goes to poll/UI but is **never** delivered to the embed `events()`
    /// reaction channel, so an agent is never woken by its own patch (alternation
    /// stays loop-safe without a per-consumer guard). On the CLI/Monitor path the
    /// self-skip is the Monitor's job.
    pub(crate) fn state_changed(
        &self,
        channel: crate::protocol::Channel,
        event: &Message,
        document: &serde_json::Value,
        is_self: bool,
    ) {
        let make = || OutputEvent::StateChanged {
            channel,
            event: Box::new(event.clone()),
            document: document.clone(),
            is_self,
        };
        match self {
            Output::Stream { mode, tap, .. } => {
                match mode {
                    OutputMode::Human => self.print_state_human(channel, event, is_self),
                    OutputMode::Json => {
                        emit(&json::format_state_json(channel, event, document, is_self));
                    }
                    OutputMode::Silent => {}
                }
                if let Some(tap) = tap {
                    let _ = tap.send(make());
                }
            }
            Output::Capture { tx, tap, .. } => {
                let evt = make();
                // Poll/fetch history + human UI always see it.
                if let Some(tap) = tap {
                    let _ = tap.send(evt.clone());
                }
                // F5: never wake an agent on its own patch.
                if !is_self {
                    let _ = tx.send(evt);
                }
            }
        }
    }

    fn print_state_human(&self, channel: crate::protocol::Channel, event: &Message, is_self: bool) {
        let what = json::state_change_summary_from_body(event.body.as_str());
        let ch = channel.label();
        if is_self {
            eprintln!("🐝️ you changed {what} ({ch})");
        } else {
            let (open, close) = self.nick_ansi(event.author.as_str(), stderr_color());
            eprintln!("🐝️ {open}<{}>{close} changed {what} ({ch})", event.author);
        }
    }

    pub(crate) fn print_presence(&self, msg: &Message) {
        let MessageKind::Presence { subtype } = &msg.kind else {
            return;
        };
        self.dispatch(
            || OutputEvent::Presence {
                msg: Box::new(msg.clone()),
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(msg.author.as_str(), stderr_color());
                    eprintln!("{open}<{}>{close} has {subtype}", msg.author);
                }
                OutputMode::Json => emit(&format_presence_json(msg, *subtype)),
                OutputMode::Silent => {}
            },
        );
    }

    /// Emit a peer-timeout event. Distinct from `presence left` (a
    /// voluntary departure): this fires when the local heartbeat
    /// sweeper evicts a participant silent past `ALIVE_TIMEOUT_SECS`.
    pub(crate) fn peer_timeout(&self, nickname: &Nickname, last_seen_secs_ago: u64) {
        self.dispatch(
            || OutputEvent::PeerTimeout {
                nickname: nickname.clone(),
                last_seen_secs_ago,
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(nickname.as_str(), stderr_color());
                    eprintln!("{open}<{nickname}>{close} went quiet");
                }
                OutputMode::Json => emit_json(&SimpleEvent::PeerTimeout {
                    nickname: nickname.as_str(),
                    last_seen_secs_ago,
                    display: peer_timeout_display(nickname.as_str()),
                }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Equivocation / fork alert (Phase 2): an author's signing key produced
    /// two different messages at the same `seq` — cryptographic proof of a
    /// fork. Surfaced once per offending key. Human mode prints a stderr
    /// warning; JSON mode emits a `fork` event for agents.
    pub(crate) fn fork(&self, nickname: &Nickname, pubkey: &str, seq: u64) {
        self.dispatch(
            || OutputEvent::Fork {
                nickname: nickname.clone(),
                pubkey: pubkey.to_owned(),
                seq,
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(nickname.as_str(), stderr_color());
                    eprintln!(
                        "⚠ fork: {open}<{nickname}>{close} signed conflicting messages at seq {seq} (key {})",
                        &pubkey[..pubkey.len().min(16)]
                    );
                }
                OutputMode::Json => emit_json(&SimpleEvent::Fork {
                    nickname: nickname.as_str(),
                    pubkey,
                    seq,
                }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Symmetric counterpart to `peer_timeout`: fires when a peer we
    /// previously evicted for silence resumes sending (any message,
    /// including an `Alive` keepalive).
    pub(crate) fn peer_return(&self, nickname: &Nickname) {
        self.dispatch(
            || OutputEvent::PeerReturn {
                nickname: nickname.clone(),
            },
            |mode| match mode {
                OutputMode::Human => {
                    let (open, close) = self.nick_ansi(nickname.as_str(), stderr_color());
                    eprintln!("{open}<{nickname}>{close} came back");
                }
                OutputMode::Json => emit_json(&SimpleEvent::PeerReturn {
                    nickname: nickname.as_str(),
                    display: peer_return_display(nickname.as_str()),
                }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print a "message posted" confirmation with the message ID.
    pub(crate) fn msg_posted(&self, id: &MessageId) {
        self.dispatch(
            || OutputEvent::MsgPosted { id: id.clone() },
            |mode| match mode {
                OutputMode::Human => {
                    eprintln!("message posted");
                    println!("{id}");
                }
                OutputMode::Json => emit_json(&SimpleEvent::MsgPosted { id: id.as_str() }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an informational line.
    pub(crate) fn info(&self, msg: &str) {
        self.dispatch(
            || OutputEvent::Info {
                message: msg.to_owned(),
            },
            |mode| match mode {
                OutputMode::Human => eprintln!("{}", self.highlight(msg, stderr_color())),
                OutputMode::Json => emit_json(&SimpleEvent::Info { message: msg }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Print an error line.
    pub(crate) fn error(&self, msg: &str) {
        self.dispatch(
            || OutputEvent::Error {
                message: msg.to_owned(),
            },
            |mode| match mode {
                OutputMode::Human => eprintln!("error: {msg}"),
                OutputMode::Json => emit_json(&SimpleEvent::Error { message: msg }),
                OutputMode::Silent => {}
            },
        );
    }

    /// Surface any displayable error (`anyhow::Error`, `BodyError`, …)
    /// through the same sink as [`Output::error`] — the single funnel
    /// for interactive (and future) error reporting, so the
    /// `error:`-prefixed Human line and the structured `Error` event
    /// stay in lockstep without each caller stringifying by hand.
    pub(crate) fn report_error(&self, error: &impl std::fmt::Display) {
        self.error(&error.to_string());
    }

    /// Emit the result of an `ahsw ping` round: per-peer RTT, plus how
    /// many of the known peers responded (the responder count is just
    /// `peers.len()`). `known` is the current participant roster size.
    pub(crate) fn ping_report(&self, peers: Vec<PingPeer>, known: usize) {
        // Not routed through `dispatch`: `peers` is owned and each arm
        // consumes (or drops) it, so there is nothing to clone.
        match self {
            Output::Capture { tx, tap, .. } => {
                if let Some(tap) = tap {
                    let _ = tap.send(OutputEvent::PingReport {
                        peers: peers.clone(),
                        known,
                    });
                }
                let _ = tx.send(OutputEvent::PingReport { peers, known });
            }
            Output::Stream { mode, tap, .. } => {
                match mode {
                    OutputMode::Human => {
                        for peer in &peers {
                            let (open, close) =
                                self.nick_ansi(peer.nickname.as_str(), stderr_color());
                            eprintln!("{open}<{}>{close} {}ms", peer.nickname, peer.rtt_ms);
                        }
                        eprintln!("{}/{known} online", peers.len());
                    }
                    OutputMode::Json => emit_json(&SimpleEvent::PingReport {
                        responded: peers.len(),
                        display: ping_report_display(&peers, known),
                        peers: peers.clone(),
                        known,
                    }),
                    OutputMode::Silent => {}
                }
                // Mirror into the daemon's surfaced-events ring so `poll`
                // can return the same `ping_report` the stream just showed.
                if let Some(tap) = tap {
                    let _ = tap.send(OutputEvent::PingReport { peers, known });
                }
            }
        }
    }

    /// Render a message in Human mode: `<author>: body`, or
    /// `<author> → <target>: body` for a reply. The author/target
    /// `<nick>` tokens are colored (self vs peer); the **body** is
    /// printed raw — it may legitimately contain `<`, `>`, `#`, so it
    /// must never be run through the colorizer.
    fn print_message_human(&self, msg: &Message) {
        let enabled = stdout_color();
        let (open, close) = self.nick_ansi(msg.author.as_str(), enabled);
        match &msg.kind {
            MessageKind::Msg {
                reply: Some(target),
            } => {
                let (to_open, to_close) = self.nick_ansi(target.as_str(), enabled);
                println!(
                    "{open}<{}>{close} → {to_open}<{}>{to_close}: {}",
                    msg.author, target, msg.body
                );
            }
            MessageKind::Msg { reply: None }
            | MessageKind::Presence { .. }
            | MessageKind::PeerInfo
            | MessageKind::Digest
            | MessageKind::StateDigest
            | MessageKind::MetaDigest
            | MessageKind::Ping
            | MessageKind::Pong { .. }
            | MessageKind::State
            | MessageKind::Meta
            | MessageKind::Task { .. } => {
                println!("{open}<{}>{close}: {}", msg.author, msg.body);
            }
        }
    }

    /// Clear the last line typed by the user (move up + erase). Human
    /// mode only; a no-op for JSON/Silent/Capture (terminal control
    /// has no structured form).
    pub(crate) fn clear_input_line(&self) {
        if let Output::Stream {
            mode: OutputMode::Human,
            ..
        } = self
        {
            eprint!("\x1b[1A\x1b[2K");
        }
    }
}
