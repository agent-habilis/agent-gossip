//! `agent-gossip a2a discover`: open a named directory, collect advertised a2a bridge
//! tickets, and either run the live picker (binding a local endpoint on the
//! chosen bridge) or stream `ticket_found`/`ticket_lost` JSON for an agent. The
//! collector lives in [`crate::a2a::TicketDirectory`]; the terminal machinery in
//! [`super::picker`].

use std::future::Future;

use anyhow::Result;

use crate::a2a::{TicketDirectory, TicketDirectoryEvent, TicketListing};
use agent_habilis_gossip::protocol::swarm::{DEFAULT_DIRECTORY, LookupSet, SwarmName};

use super::picker::{self, PickerOutcome, PickerText, interrupted, sigterm_stream};

/// How much of a ticket the picker shows — enough to tell entries apart, not the
/// whole multi-hundred-char token (the pick carries the full one).
const TICKET_PREVIEW_CHARS: usize = 16;

/// Run `agent-gossip a2a discover`: browse `directory` for advertised bridges. Human +
/// TTY runs the picker and, on a pick, binds the local bridge (`a2a connect`);
/// `json` (or no TTY) streams directory changes and returns.
///
/// # Errors
/// The directory session cannot be established, or the post-pick connect fails.
pub(super) async fn discover(
    directory: Option<SwarmName>,
    port: Option<u16>,
    lookups: LookupSet,
    password: Option<super::password::PasswordFlag>,
    json: bool,
) -> Result<()> {
    let name = directory.unwrap_or_else(|| {
        SwarmName::new(DEFAULT_DIRECTORY).expect("the default directory name is valid")
    });
    let mut discoverer = TicketDirectory::open(name.clone(), lookups).await?;
    // Route the directory session's logs to its per-member file so the picker /
    // JSON stream stays clean.
    if let Some((swarm, nickname)) = discoverer.session_identity() {
        agent_habilis_gossip::logging::attach(swarm, nickname);
    }
    let mut events = discoverer
        .events()
        .expect("ticket discoverer events receiver is available exactly once");

    if !json {
        match run_ticket_picker(name.as_str(), &discoverer, &mut events).await {
            PickerOutcome::Selected(ticket) => {
                let _ = discoverer.close().await;
                // Leave the directory's log file behind so the connect path logs
                // under its own identity.
                agent_habilis_gossip::logging::detach();
                let password = resolve_pick_password(password, &ticket)?;
                return interruptible(crate::a2a::connect(&ticket, port, json, password)).await;
            }
            PickerOutcome::Quit => {
                let _ = discoverer.close().await;
                return Ok(());
            }
            // No TTY for raw mode — fall through to the streaming view.
            PickerOutcome::Unsupported => {}
        }
    }

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
    Ok(())
}

/// Resolve the password for a picked ticket: prompt when the ticket needs one
/// and none was passed, otherwise honor the flag.
fn resolve_pick_password(
    password: Option<super::password::PasswordFlag>,
    ticket: &str,
) -> Result<Option<agent_habilis_gossip::protocol::crypto::Password>> {
    match password {
        None if crate::a2a::ticket_requires_password(ticket) => {
            Ok(Some(super::password::require_password(false, "ticket")?))
        }
        other => super::password::resolve_password(other, /* confirm */ false, false),
    }
}

/// Run the post-pick connect under explicit signal handling — the picker
/// registered process-wide signal handlers (it needs SIGTERM to restore the
/// terminal), which suppresses the OS default-terminate, so the connect that
/// follows must listen for itself or Ctrl-C would no longer kill it.
async fn interruptible(work: impl Future<Output = Result<()>>) -> Result<()> {
    let mut work = Box::pin(work);
    let mut sigterm = sigterm_stream();
    tokio::select! {
        result = &mut work => result,
        () = interrupted(&mut sigterm) => anyhow::bail!("interrupted"),
    }
}

/// One ticket-directory change as a JSON line. `Found`/`Updated` both surface as
/// `ticket_found` (upsert semantics); a departure is `ticket_lost`. The full
/// ticket is the machine product — strip nothing.
fn ticket_event_json(event: &TicketDirectoryEvent) -> String {
    let value = match event {
        TicketDirectoryEvent::Found(listing) | TicketDirectoryEvent::Updated(listing) => {
            serde_json::json!({
                "event": "ticket_found",
                "ticket": listing.ticket,
                "label": listing.label,
                "password": listing.password,
            })
        }
        TicketDirectoryEvent::Lost(ticket) => serde_json::json!({
            "event": "ticket_lost",
            "ticket": ticket,
        }),
    };
    value.to_string()
}

/// Drive the shared picker with ticket rendering: the label (or the ticket's
/// leading chars when unlabeled) in yellow, a ticket preview, and the local
/// first-seen timestamp; `enter` connects.
async fn run_ticket_picker(
    directory_label: &str,
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
        empty: "waiting for a2a bridges".to_owned(),
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
        let lock = if listing.password { "🔒 " } else { "" };
        format!(
            "{bold}{yellow}{label}{reset}  {lock}{preview}…  {}",
            agent_habilis_gossip::util::clock::local_datetime(listing.first_seen_unix),
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
