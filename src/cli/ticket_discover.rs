//! The shared ticket `discover` flow behind `pipe discover` / `file
//! discover` / `port discover`: open a named directory, collect ticket ads
//! of one kind, and either run the live picker (returning the chosen
//! ticket for the caller to connect with) or stream
//! `ticket_found`/`ticket_lost` JSON lines for an agent. The pure
//! primitives live in [`crate::directory::ticket`]; the live consumer in
//! [`crate::embed::TicketDirectory`]; the terminal machinery in
//! [`super::picker`].

use std::future::Future;

use anyhow::Result;

use crate::directory::ticket::TicketListing;
use crate::embed::{TicketDirectory, TicketDirectoryEvent};
use crate::protocol::swarm::{LookupSet, SwarmName};
use crate::protocol::token::TokenType;

use super::picker::{self, PickerOutcome, PickerText, interrupted, sigterm_stream};

/// How much of a ticket the picker shows — enough to tell entries apart,
/// not the whole multi-hundred-char token (the pick carries the full one).
const TICKET_PREVIEW_CHARS: usize = 16;

/// Run the post-pick connect under explicit signal handling. The discover
/// UI already registered process-wide signal handlers (the picker needs
/// SIGTERM to restore the terminal), which suppresses the OS
/// default-terminate forever — so the connect that follows must listen for
/// itself or ctrl-c would no longer kill it. First ctrl-c/SIGTERM
/// interrupts the transfer with a non-zero "interrupted" error.
///
/// # Errors
/// The wrapped connect's error, or `interrupted` on a termination signal.
pub(super) async fn interruptible(work: impl Future<Output = Result<()>>) -> Result<()> {
    // Boxed so this wrapper's future stays small (clippy `large_futures`) —
    // the connect futures it wraps are already near the 16 KiB budget.
    let mut work = Box::pin(work);
    let mut sigterm = sigterm_stream();
    tokio::select! {
        result = &mut work => result,
        () = interrupted(&mut sigterm) => anyhow::bail!("interrupted"),
    }
}

/// Browse directory `name` for advertised tickets of `kind`. Human + TTY
/// runs the live picker and returns the chosen full ticket (`None` on
/// quit); `json` (or no TTY) streams `ticket_found`/`ticket_lost` JSON
/// lines until interrupted and returns `None` — the agent captures a
/// ticket and connects itself.
///
/// # Errors
/// The directory session cannot be established (endpoint/gossip setup,
/// bootstrap unreachable).
pub(super) async fn discover_ticket(
    name: SwarmName,
    lookups: LookupSet,
    kind: TokenType,
    json: bool,
) -> Result<Option<String>> {
    let mut discoverer = TicketDirectory::open(name.clone(), lookups, kind).await?;
    // Route the directory session's logs to its per-member file (same as
    // `discover`) so the picker / JSON stream stays clean.
    if let Some((swarm, nickname)) = discoverer.session_identity() {
        crate::logging::attach(swarm, nickname);
    }
    let mut events = discoverer
        .events()
        .expect("ticket discoverer events receiver is available exactly once");

    if !json {
        match run_ticket_picker(name.as_str(), kind, &discoverer, &mut events).await {
            PickerOutcome::Selected(ticket) => {
                let _ = discoverer.close().await;
                // Leave the directory's log file behind so the connect
                // path logs under its own identity.
                crate::logging::detach();
                return Ok(Some(ticket));
            }
            PickerOutcome::Quit => {
                let _ = discoverer.close().await;
                return Ok(None);
            }
            // No TTY for raw mode — fall through to the streaming view.
            PickerOutcome::Unsupported => {}
        }
    }

    // Agent / non-TTY mode: one JSON line per directory change until
    // interrupted; the agent connects with a captured ticket itself.
    let mut sigterm = sigterm_stream();
    loop {
        tokio::select! {
            () = interrupted(&mut sigterm) => break,
            received = events.recv() => match received {
                Some(change) => println!("{}", ticket_event_json(&change)),
                None => break,
            },
        }
    }
    let _ = discoverer.close().await;
    Ok(None)
}

/// One ticket-directory change as a JSON line. `Found`/`Updated` both
/// surface as `ticket_found` (upsert semantics, like `swarm_found`); a
/// departure is `ticket_lost`. The full ticket is the machine product —
/// strip nothing.
fn ticket_event_json(event: &TicketDirectoryEvent) -> String {
    let value = match event {
        TicketDirectoryEvent::Found(listing) | TicketDirectoryEvent::Updated(listing) => {
            serde_json::json!({
                "event": "ticket_found",
                "ticket": listing.ticket,
                "label": listing.label,
            })
        }
        TicketDirectoryEvent::Lost(ticket) => serde_json::json!({
            "event": "ticket_lost",
            "ticket": ticket,
        }),
    };
    value.to_string()
}

/// The picker noun for a ticket kind (`waiting for pipes…`).
fn kind_noun(kind: TokenType) -> &'static str {
    match kind {
        TokenType::Pipe => "pipes",
        TokenType::File => "files",
        TokenType::Port => "ports",
        TokenType::Mount => "mounts",
        TokenType::Swarm => "swarms",
        TokenType::Sh => "shells",
    }
}

/// Drive the shared picker with ticket rendering: the label (or the
/// ticket's leading chars when unlabeled) in yellow, a ticket preview,
/// and the local first-seen timestamp; `enter` connects.
async fn run_ticket_picker(
    directory_label: &str,
    kind: TokenType,
    discoverer: &TicketDirectory,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<TicketDirectoryEvent>,
) -> PickerOutcome {
    use crate::output::style;

    let (yellow, reset) = if crate::output::stdout_color() {
        (style::SWARM, style::RESET)
    } else {
        ("", "")
    };
    let text = PickerText {
        header: format!("discovering {yellow}#{directory_label}{reset} directory"),
        empty: format!("waiting for {}", kind_noun(kind)),
        footer: "↑/↓ move · enter connect · q quit".to_owned(),
    };
    let row = |listing: &TicketListing, selected: bool| {
        let bold = if selected && !yellow.is_empty() {
            style::BOLD
        } else {
            ""
        };
        let preview: String = listing.ticket.chars().take(TICKET_PREVIEW_CHARS).collect();
        let label = listing.label.as_deref().unwrap_or("(unlabeled)");
        format!(
            "{bold}{yellow}{label}{reset}  {preview}…  {}",
            crate::util::clock::local_datetime(listing.first_seen_unix),
        )
    };
    picker::run(
        &text,
        || discoverer.snapshot(),
        row,
        |listing| listing.ticket.clone(),
        events,
    )
    .await
}
