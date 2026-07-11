//! The `discover` subcommand: browse a directory's live meshes.
//!
//! Human + TTY renders a live arrow-key picker that hands off to `join`
//! on selection; `--no-interactive` / `--output json` streams
//! `square_found`/`square_lost` JSON lines for an agent to act on. The pure
//! directory primitives live in [`agent_habilis_mesh::directory`]; the live consumer
//! in [`crate::api::Directory`]; the terminal machinery in
//! [`super::picker`]; this file is just the CLI command + mesh rendering.

use anyhow::Result;

use crate::api::{Directory, DirectoryEvent, MeshListing};
use agent_habilis_mesh::resolver::JoinTarget;

use super::args::{DiscoverOpts, OutputFormat};
use super::join;
use super::picker::{self, PickerOutcome, PickerText, interrupted, sigterm_stream};

/// Browse a directory. Human mode renders a live picker that
/// hands off to `join` on selection; `--no-interactive` / `--output
/// json` streams `square_found`/`square_lost` JSON lines instead (the
/// agent picks and joins by id itself).
pub(super) async fn discover(opts: DiscoverOpts) -> Result<()> {
    let directory_label = opts
        .directory
        .as_ref()
        .map_or_else(|| "global".to_owned(), |name| name.as_str().to_owned());
    // Reach the directory over the chosen lookups (default all-on). Must
    // match the lookups the advertiser used — an mDNS-only advertiser is
    // found only by an mDNS-only `discover`.
    let mut discoverer = Directory::open(
        opts.directory.map(|name| name.as_str().to_owned()),
        opts.lookups.to_set(),
    )
    .await?;
    // Route the directory session's logs to its per-member file (same as
    // create/join) so the picker and JSON stream aren't drowned in INFO
    // lines on stderr.
    if let Some((mesh, nickname)) = discoverer.session_identity() {
        agent_habilis_mesh::logging::attach(mesh, nickname);
    }
    let mut events = discoverer
        .events()
        .expect("discoverer events receiver is available exactly once");

    // Human + a real terminal ⇒ the live arrow-key picker; otherwise
    // (agent / piped) stream JSON changes.
    let want_picker = !opts.shared.no_interactive && opts.shared.output == OutputFormat::Human;
    if want_picker {
        match run_mesh_picker(&directory_label, &discoverer, &mut events).await {
            PickerOutcome::Selected(id) => {
                let _ = discoverer.close().await;
                // Leave the directory's log file behind so `join` opens
                // the joined mesh's own file (with its setup logs) rather
                // than appending to the directory session's.
                agent_habilis_mesh::logging::detach();
                let target = id
                    .parse::<JoinTarget>()
                    .expect("a discovered mesh id is a valid join target");
                // No flag on `discover` itself: a passworded pick prompts
                // in `join` (the picker only ran because a TTY exists). Boxed
                // to keep this future under the `large_futures` threshold.
                return Box::pin(join(target, None, None, opts.shared)).await;
            }
            PickerOutcome::Quit => {
                let _ = discoverer.close().await;
                return Ok(());
            }
            // No TTY for raw mode — fall through to the streaming view.
            PickerOutcome::Unsupported => {}
        }
    }

    // Agent / non-TTY mode: one JSON line per directory change until
    // interrupted (SIGINT or SIGTERM — see `interrupted`).
    let mut sigterm = sigterm_stream();
    loop {
        tokio::select! {
            () = interrupted(&mut sigterm) => break,
            received = events.recv() => match received {
                Some(change) => println!("{}", discover_event_json(&change)),
                None => break,
            },
        }
    }
    let _ = discoverer.close().await;
    Ok(())
}

/// One directory change as a JSON line for `agent-square discover --output json`.
/// `Found`/`Updated` both surface as `square_found` (upsert semantics —
/// the agent treats a re-ad as a refresh); a departure is `square_lost`.
fn discover_event_json(event: &DirectoryEvent) -> String {
    let value = match event {
        DirectoryEvent::Found(listing) | DirectoryEvent::Updated(listing) => serde_json::json!({
            "event": "square_found",
            "square": listing.mesh.as_str(),
            "name": listing.name,
            "mode": if listing.public { "public" } else { "private" },
            "password": listing.password,
            "peers": listing.peers,
        }),
        DirectoryEvent::Lost(mesh) => serde_json::json!({
            "event": "square_lost",
            "square": mesh.as_str(),
        }),
    };
    value.to_string()
}

/// Drive the shared picker with mesh rendering: the directory + each
/// mesh name in yellow, the full `💬…` id, peer count, and a local
/// first-seen timestamp; `enter` joins the highlighted mesh.
async fn run_mesh_picker(
    directory_label: &str,
    discoverer: &Directory,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<DirectoryEvent>,
) -> PickerOutcome {
    use crate::output::style;

    // Reuse the shared mesh-name color, gated on a TTY + `NO_COLOR`
    // like every other colored path. Empty strings when off.
    let (yellow, reset) = if crate::output::stdout_color() {
        (style::MESH, style::RESET)
    } else {
        ("", "")
    };
    let text = PickerText {
        header: format!("discovering {yellow}#{directory_label}{reset} directory"),
        empty: "waiting for squares".to_owned(),
        footer: "↑/↓ move · enter join · q quit".to_owned(),
    };
    let row = |listing: &MeshListing, selected: bool| {
        let bold = if selected && !yellow.is_empty() {
            style::BOLD
        } else {
            ""
        };
        let lock = if listing.password { "🔒 " } else { "" };
        format!(
            "{bold}{yellow}#{}{reset}  {lock}{}  {}  {}",
            listing.name,
            listing.mesh.as_str(),
            listing.peers,
            agent_habilis_mesh::util::clock::local_datetime(listing.first_seen_unix),
        )
    };
    picker::run(
        &text,
        picker::PickerCallbacks {
            snapshot: || discoverer.snapshot(),
            row,
            pick: |listing: &MeshListing| listing.mesh.as_str().to_owned(),
        },
        events,
    )
    .await
}
