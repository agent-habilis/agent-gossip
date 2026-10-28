use anyhow::Result;

use super::a2a::a2a;
use super::args::{Cli, Commands};
use super::create::create;
use super::install_tuning::install_tuning;
use super::invite::invite;
use super::join::join;
use super::meta::meta;
use super::peers::peers;
use super::ping::ping;
use super::poll::poll;
use super::ready_state_file;
use super::reject_id_encoded_flag::reject_id_encoded_flag;
use super::state::state;
use super::topic::topic;
use super::topology_cmd::topology_cmd;

/// Run the selected subcommand to completion.
///
/// # Errors
/// Propagates any error from the selected subcommand — mesh setup
/// failure, join timeout, IPC errors, invalid mesh-mode flags, etc.
pub(crate) async fn dispatch(cli: Cli) -> Result<()> {
    // Install the log-path config from the global flags before any
    // subcommand resolves its log file (the buffered sink flushes at
    // `logging::attach`, after this). Replaces the old AHS_LOG_DIR /
    // AHS_LOG_MAX_BYTES env reads.
    habilis_network::util::logs::configure(habilis_network::util::logs::LogConfig {
        base: Some(crate::runtime_base()),
        dir: cli.log_dir,
        max_bytes: cli.log_max_bytes,
        raw: cli.log_raw,
    });
    // Source the process tuning from the (hidden) flags before any handler
    // resolution reads it — e.g. `create --advertise` checks the
    // `directory_private` guard while building its `SetupKind`, before the
    // event loop. Replaces the former env-var overrides.
    match cli.command {
        // The event-loop futures (`create`/`join`/`discover`/`mcp::run`) are
        // boxed: each holds a full `EventLoopState`/session, so they sit right at
        // clippy's `large_futures` 16 KiB threshold — boxing keeps the dispatch
        // future small and the size off the knife's edge as those types grow.
        Commands::Create { options } => {
            install_tuning(&options.shared.tuning);
            Box::pin(create(options)).await
        }
        Commands::Join { options } => {
            reject_id_encoded_flag("--name", options.name.is_some())?;
            install_tuning(&options.shared.tuning);
            Box::pin(join(
                options.gossip,
                options.nickname,
                options.password,
                options.shared,
            ))
            .await
        }
        Commands::Topic { options } => {
            install_tuning(&options.shared.tuning);
            Box::pin(topic(options)).await
        }
        Commands::Leave { options } => super::session::leave(options).await,
        Commands::Session { options } => super::session::session(options).await,
        Commands::BellCheck { options } => {
            if super::session::bell_check(&options) {
                // 3, not 1 or 2: clap's usage error is 2, and 1 is any error.
                std::io::Write::flush(&mut std::io::stdout())?;
                std::process::exit(3);
            }
            Ok(())
        }
        Commands::Poll { options } => poll(options).await,
        Commands::Ping { options } => ping(options).await,
        Commands::A2a { options } => Box::pin(a2a(options.action)).await,
        Commands::Peers { options } => peers(options).await,
        Commands::Invite { options } => invite(options).await,
        Commands::State { options } => state(options).await,
        Commands::Meta { options } => meta(options).await,
        Commands::Topology { options } => topology_cmd(options).await,
        Commands::Ready { options } => ready_state_file::ready(options).await,
        Commands::Discover { options } => {
            install_tuning(&options.tuning);
            Box::pin(super::discover::discover(options)).await
        }
        Commands::Mcp {
            directory_private,
            ping_window_secs,
            longpoll_max_ms,
        } => {
            // The MCP server holds no `SharedServerOpts`; install just the
            // hidden knobs the suite varies (loopback directory, short ping
            // window, short long-poll park) over the production defaults.
            habilis_network::runtime::tuning::init(habilis_network::runtime::tuning::Tuning {
                ping_window: std::time::Duration::from_secs(ping_window_secs),
                directory_private,
                ..habilis_network::runtime::tuning::Tuning::DEFAULTS
            });
            crate::a2a::tuning::init(crate::a2a::tuning::Tuning {
                longpoll_max_ms,
                ..crate::a2a::tuning::Tuning::DEFAULTS
            });
            Box::pin(crate::mcp::run()).await
        }
        // The manual is embedded at compile time (`include_str!`), so the
        // binary documents itself with no repo checkout.
        Commands::Man => {
            print!("{}", include_str!("../../../../docs/manual.txt"));
            Ok(())
        }
        // Embedded integration artifacts written to the agents' skills dirs —
        // self-contained, no repo checkout needed (like `Man`).
        Commands::Plug { agents, paths } => super::plug::plug(&agents, &paths),
        Commands::Unplug { agents, paths } => super::plug::unplug(&agents, &paths),
        Commands::Doctor { options } => super::doctor::run(options).await,
    }
}
