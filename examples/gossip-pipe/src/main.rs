//! `gossip-pipe` — a Unix pipe that carries stdin bytes over the gossip mesh.
//!
//! The **second consumer** of the `agent-habilis-gossip` engine, and a
//! deliberately non-a2a one: it defines its own two-tag `App` taxonomy
//! (`pipe_data` / `pipe_eof`) and a [`MeshApp`]/[`MeshDriver`] that turns
//! inbound frames into stdout bytes — proving the engine is payload-generic (it
//! carries a2a tasks and raw bytes over the *same* mesh/discovery/circuit stack,
//! never inspecting either payload). It depends only on the engine crate, never
//! on `agent-gossip`.
//!
//! `listen` reads stdin, base64s each ≤`--chunk` slice into a `pipe_data` frame,
//! and emits one `pipe_eof` at EOF. `connect` joins the same swarm and streams
//! every inbound `pipe_data` to stdout, exiting on `pipe_eof`. Base64 is used
//! because [`MessageBody`] is text/JSON-shaped (rejects control bytes); a raw
//! byte stream therefore rides base64-encoded in the frame body.

use std::time::Duration;

use agent_habilis_gossip::async_trait;
use agent_habilis_gossip::daemon::app::MeshDriver;
use agent_habilis_gossip::daemon::ctx::HandlerCtx;
use agent_habilis_gossip::daemon::setup::{SetupKind, SetupParams, setup_swarm};
use agent_habilis_gossip::daemon::state::EventLoopState;
use agent_habilis_gossip::daemon::{CreateParams, JoinParams, Resolved, TopicParams};
use agent_habilis_gossip::embed::MeshNode;
use agent_habilis_gossip::gossip::app::{AppClass, MeshApp};
use agent_habilis_gossip::gossip::event::SilentSink;
use agent_habilis_gossip::protocol::swarm::{
    DirectorySelection, LookupSet, RelaySelection, SwarmConfig, SwarmName, resolve_lookups,
};
use agent_habilis_gossip::protocol::{AppTag, Message, MessageBody, Nickname};
use agent_habilis_gossip::resolver::JoinTarget;
use agent_habilis_gossip::util::consts::{GOSSIP_ACTIVE_VIEW_CAPACITY, MAX_MESSAGE_SIZE};
use agent_habilis_gossip::util::tuning::{self, Tuning};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::{Args, Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

/// This app's `App`-frame tag taxonomy — its own namespace, mirroring how the
/// a2a layer owns `a2a_msg` / `a2a_status` / … . The engine routes on the tag
/// but never interprets it.
mod tag {
    /// One ordered slice of the byte stream (base64 in the frame body).
    pub(crate) const DATA: &str = "pipe_data";
    /// The source ended: the consumer flushes stdout and exits 0.
    pub(crate) const EOF: &str = "pipe_eof";
}

#[derive(Parser)]
#[command(
    name = "gossip-pipe",
    about = "A Unix pipe over the gossip mesh (a non-a2a consumer of agent-habilis-gossip)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read stdin and stream it over the mesh as `pipe_data` frames.
    Listen(ListenArgs),
    /// Join the swarm and write inbound `pipe_data` frames to stdout.
    Connect(ConnectArgs),
}

/// Process-global transport policy, shared by both subcommands. The engine's
/// directed-send tier order is unicast → circuit → gossip; on loopback the
/// addressee is always dialable, so unicast wins and the circuit never triggers.
/// Turning unicast and the gossip-directed fallback off — with a capped active
/// view so the mesh is genuinely partial — forces directed frames onto a
/// multi-hop circuit. Defaults reproduce the pre-existing behaviour exactly
/// (unicast on, gossip-directed on, circuit on, full active view).
#[derive(Args, Clone, Copy)]
struct TransportArgs {
    /// Cap on active-view neighbours (a small cap forces a partial mesh, so
    /// routes to non-neighbours become multi-hop).
    #[arg(long, value_name = "N", default_value_t = GOSSIP_ACTIVE_VIEW_CAPACITY)]
    max_peers: usize,
    /// Disable the unicast (point-to-point) directed transport.
    #[arg(long)]
    no_unicast: bool,
    /// Disable gossip as a directed transport / fallback.
    #[arg(long)]
    no_gossip_directed: bool,
}

impl TransportArgs {
    /// Install this policy as the process-global tuning. The circuit stays on so
    /// a directed frame with no unicast/gossip path still routes multi-hop.
    fn install(self) {
        tuning::init(Tuning {
            unicast_enabled: !self.no_unicast,
            gossip_directed_enabled: !self.no_gossip_directed,
            circuit_enabled: true,
            ..Tuning::DEFAULTS
        });
    }
}

#[derive(Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent CLI discovery flags (--public/--mdns/--dht/--relay) mirroring the agent-gossip CLI; they are flat clap flags, not a state machine to model as an enum"
)]
struct ListenArgs {
    /// Join an existing swarm by its 🐝… id.
    #[arg(long, value_name = "🐝…")]
    swarm: Option<String>,
    /// Derive a public swarm from a shared string (both sides pass the same).
    #[arg(long, value_name = "STRING")]
    topic: Option<String>,
    /// Create a public swarm (all-on discovery) and print its id.
    #[arg(long)]
    public: bool,
    /// Create a swarm discoverable over mDNS.
    #[arg(long)]
    mdns: bool,
    /// Create a swarm discoverable over the mainline DHT.
    #[arg(long)]
    dht: bool,
    /// Create a swarm reachable via the default relay ladder.
    #[arg(long)]
    relay: bool,
    /// Send directed frames to exactly this peer (else broadcast to all).
    #[arg(long, value_name = "NICK")]
    to: Option<String>,
    /// Max raw bytes per frame (default sized to one gossip frame).
    #[arg(long, value_name = "N")]
    chunk: Option<usize>,
    /// Local nickname (default: a random one).
    #[arg(long, value_name = "NAME")]
    nick: Option<String>,
    #[command(flatten)]
    transport: TransportArgs,
}

#[derive(Args)]
struct ConnectArgs {
    /// The swarm 🐝… id to join (omit when using `--topic`).
    #[arg(value_name = "🐝…")]
    id: Option<String>,
    /// Derive the same public swarm from a shared string as the sender.
    #[arg(long, value_name = "STRING")]
    topic: Option<String>,
    /// Local nickname (default: a random one).
    #[arg(long, value_name = "NAME")]
    nick: Option<String>,
    #[command(flatten)]
    transport: TransportArgs,
}

/// The engine seam. `connect` writes inbound `pipe_data` to `stdout` and fires
/// `done` on `pipe_eof`; `listen` constructs it with neither (it only sends).
struct PipeApp {
    stdout: tokio::io::Stdout,
    /// Fired once, on `pipe_eof`, so the `connect` main task can exit 0.
    done: Option<oneshot::Sender<()>>,
}

impl PipeApp {
    /// The receive-side app: streams inbound bytes to stdout, signals `done` on EOF.
    fn receiver(done: oneshot::Sender<()>) -> Self {
        Self {
            stdout: tokio::io::stdout(),
            done: Some(done),
        }
    }

    /// The send-side app: it only emits frames, so it never touches stdout/`done`.
    fn sender() -> Self {
        Self {
            stdout: tokio::io::stdout(),
            done: None,
        }
    }
}

/// The typed in-process request the `listen` loop pushes: one outbound frame.
enum PipeRequest {
    Send {
        tag: AppTag,
        to: Option<Nickname>,
        body: MessageBody,
    },
}

#[async_trait]
impl MeshApp for PipeApp {
    fn classify(&self, _message: &Message) -> AppClass {
        // Pipe frames are ephemeral stream bytes: never logged, never a task
        // beat, always valid (the body is opaque base64), and carry no
        // per-author hash chain. Directed frames are sent as plaintext
        // (base64, unsealed) — gossip-pipe publishes no a2a card, so it can
        // neither seal to a peer nor be sealed to — so `sealed: false`: the
        // addressee passes the plaintext body straight through to `on_app_frame`
        // instead of trying (and failing) to unseal it.
        AppClass {
            loggable: false,
            beat: false,
            valid: true,
            chained: false,
            sealed: false,
        }
    }

    async fn on_app_frame(
        &mut self,
        message: &Message,
        _surfaceable: bool,
        _state: &mut EventLoopState,
        _ctx: &HandlerCtx<'_>,
    ) -> bool {
        match message.kind.app_tag().map(AppTag::as_str) {
            Some(tag::DATA) => match BASE64.decode(message.body.as_str()) {
                Ok(bytes) => {
                    if self.stdout.write_all(&bytes).await.is_ok() {
                        let _ = self.stdout.flush().await;
                    }
                }
                Err(error) => {
                    eprintln!("gossip-pipe: dropping undecodable pipe_data: {error}");
                }
            },
            Some(tag::EOF) => {
                let _ = self.stdout.flush().await;
                if let Some(done) = self.done.take() {
                    let _ = done.send(());
                }
            }
            Some(_) | None => {}
        }
        // Pipe frames are never retained/indexed — return `false`, the frame is
        // fully handled here.
        false
    }
}

#[async_trait]
impl MeshDriver for PipeApp {
    // A minimal consumer that only *receives* would set all three to `()`. This
    // one carries `PipeRequest` so `listen` can push outbound frames; it serves
    // no localhost HTTP binding and no IPC command set, so those stay trivial.
    type Session = PipeRequest;
    type Http = ();
    type Ipc = ();

    async fn handle_session(
        &mut self,
        req: PipeRequest,
        state: &mut EventLoopState,
        ctx: &HandlerCtx<'_>,
    ) -> bool {
        let PipeRequest::Send { tag, to, body } = req;
        if let Err(error) =
            agent_habilis_gossip::gossip::send_app(state, ctx, tag, to, None, body).await
        {
            eprintln!("gossip-pipe: send failed: {error}");
        }
        true
    }
}

/// How the two subcommands select a swarm, resolved into a [`SetupKind`].
enum Select {
    /// Join a 🐝… id.
    Swarm(String),
    /// Derive a public swarm from a shared string.
    Topic(String),
    /// Create a fresh swarm over these lookups (`public` ⇒ the all-on preset).
    Create { lookups: LookupSet, public: bool },
}

impl Select {
    /// Resolve into `(SetupKind, author, is_create)`. `is_create` tells `listen`
    /// to print the minted id so a `connect` peer can join it.
    fn resolve(self, nickname: Option<Nickname>) -> Result<(SetupKind, Nickname, bool)> {
        match self {
            Select::Swarm(id) => {
                let target: JoinTarget = id.parse().map_err(|error| anyhow::anyhow!("{error}"))?;
                let Resolved { kind, author, .. } = JoinParams {
                    target,
                    nickname,
                    password: None,
                }
                .resolve()
                .context("resolving the swarm id")?;
                Ok((kind, author, false))
            }
            Select::Topic(string) => {
                let Resolved { kind, author, .. } = TopicParams { string, nickname }
                    .resolve()
                    .context("resolving the topic string")?;
                Ok((kind, author, false))
            }
            Select::Create { lookups, public } => {
                let config = SwarmConfig {
                    lookups: resolve_lookups(public, lookups),
                    password: None,
                };
                let name = SwarmName::new("gossip-pipe").expect("valid swarm name");
                let Resolved { kind, author, .. } = CreateParams {
                    name,
                    nickname,
                    config,
                    advertise: DirectorySelection::Unset,
                    password: None,
                }
                .resolve()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
                Ok((kind, author, true))
            }
        }
    }
}

/// Map the `listen` rendezvous flags to a [`Select`]. Exactly one source: an id,
/// a topic string, or a create with lookup flags (bare `listen` ⇒ loopback create).
fn listen_select(args: &ListenArgs) -> Result<Select> {
    match (&args.swarm, &args.topic) {
        (Some(_), Some(_)) => anyhow::bail!("pass only one of --swarm / --topic"),
        (Some(id), None) => Ok(Select::Swarm(id.clone())),
        (None, Some(string)) => Ok(Select::Topic(string.clone())),
        (None, None) => {
            let lookups = LookupSet {
                mdns: args.mdns,
                dht: args.dht,
                relay: if args.relay {
                    RelaySelection::Default
                } else {
                    RelaySelection::Unset
                },
            };
            Ok(Select::Create {
                lookups,
                public: args.public,
            })
        }
    }
}

/// Build the event loop for `select`, spawn it as a [`MeshNode`], and return the
/// node. `handle_signals` is true — each subcommand is a foreground process that
/// owns its own lifetime, so ctrl-c should leave the swarm cleanly.
async fn spawn_node(
    select: Select,
    nickname: Option<Nickname>,
    app: PipeApp,
    max_peers: usize,
) -> Result<MeshNode<PipeApp>> {
    let (kind, author, is_create) = select.resolve(nickname)?;
    let elc = setup_swarm(
        kind,
        SetupParams {
            author,
            interactive: false,
            max_peers,
            state_file: None,
            spool: None,
            sink: std::sync::Arc::new(SilentSink),
            drift: None,
            a2a_serve: None,
        },
    )
    .await
    .context("setting up the swarm")?;
    let node = MeshNode::spawn(elc, app, /* handle_signals */ true);
    if is_create {
        // The minted id, so a `connect` peer can join this swarm.
        eprintln!("gossip-pipe: swarm {}", node.swarm_id().as_str());
    }
    Ok(node)
}

/// The default raw-bytes-per-frame budget. `send_app` sends a single (unsharded)
/// frame, so the base64-inflated body plus the JSON envelope must fit
/// [`MAX_MESSAGE_SIZE`]. Invert base64's 4/3 growth after reserving envelope
/// headroom, then round to a multiple of 3 (so base64 emits no mid-stream
/// padding).
fn default_chunk() -> usize {
    const ENVELOPE_RESERVE: usize = 1024;
    let body_budget = MAX_MESSAGE_SIZE.saturating_sub(ENVELOPE_RESERVE);
    let raw = body_budget / 4 * 3;
    (raw / 3) * 3
}

async fn run_listen(args: ListenArgs) -> Result<()> {
    let select = listen_select(&args)?;
    let nickname = args
        .nick
        .map(Nickname::new)
        .transpose()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let to = args
        .to
        .map(Nickname::new)
        .transpose()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let chunk = args
        .chunk
        .filter(|value| *value > 0)
        .unwrap_or_else(default_chunk);

    let node = spawn_node(select, nickname, PipeApp::sender(), args.transport.max_peers).await?;

    let mut stdin = tokio::io::stdin();
    let mut buf = vec![0u8; chunk];
    loop {
        let read = stdin.read(&mut buf).await.context("reading stdin")?;
        if read == 0 {
            break;
        }
        let body = MessageBody::new(BASE64.encode(&buf[..read]))
            .expect("base64 output has no control characters");
        node.send(PipeRequest::Send {
            tag: AppTag::from(tag::DATA),
            to: to.clone(),
            body,
        })
        .await?;
    }
    // One end-of-stream marker (empty body).
    node.send(PipeRequest::Send {
        tag: AppTag::from(tag::EOF),
        to: to.clone(),
        body: MessageBody::new(String::new()).expect("empty body is valid"),
    })
    .await?;

    // Give the final frames a moment to reach the mesh before we leave (a
    // gossip broadcast is fire-and-forget; leaving instantly could race it).
    tokio::time::sleep(Duration::from_millis(750)).await;
    node.leave().await?;
    Ok(())
}

async fn run_connect(args: ConnectArgs) -> Result<()> {
    let select = match (args.id, args.topic) {
        (Some(_), Some(_)) => anyhow::bail!("pass either a 🐝… id or --topic, not both"),
        (Some(id), None) => Select::Swarm(id),
        (None, Some(string)) => Select::Topic(string),
        (None, None) => anyhow::bail!("connect needs a 🐝… id or --topic <string>"),
    };
    let nickname = args
        .nick
        .map(Nickname::new)
        .transpose()
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let (done_tx, done_rx) = oneshot::channel();
    let node = spawn_node(
        select,
        nickname,
        PipeApp::receiver(done_tx),
        args.transport.max_peers,
    )
    .await?;

    // Block until the sender's `pipe_eof` fires `done` (or the loop dies).
    let _ = done_rx.await;
    node.leave().await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Install the process-global transport policy once, before any swarm setup.
    match &cli.command {
        Command::Listen(args) => args.transport.install(),
        Command::Connect(args) => args.transport.install(),
    }
    match cli.command {
        Command::Listen(args) => run_listen(args).await,
        Command::Connect(args) => run_connect(args).await,
    }
}
