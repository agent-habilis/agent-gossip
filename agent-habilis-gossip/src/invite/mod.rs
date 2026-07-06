//! Creator-issued invites to an **invite-only** swarm. An invite is a `🎟️`
//! bearer ticket that carries the swarm's published hash, the invite **root**
//! (the derivation secret that the bare hash withholds), an expiry (TTL), and
//! the creator's signature over those fields. Only the creator — who alone
//! holds the in-memory issuer key — can mint one; any member could package the
//! root, but not a *valid* (creator-signed) invite. See [`crate::protocol::swarm`]
//! for the invite-only swarm itself.

mod ticket;

// `InviteTicket` rides the **public** `JoinTarget::Invite`, so it must be at
// least as public as that enum (re-exported at the crate root in `lib.rs`); its
// methods stay `pub(crate)`, so externally it is an opaque join token.
pub use ticket::InviteTicket;
// `pub` so the application layer's `invite` command can mint from the retained
// creator swarm (`EventLoopState::mint_swarm`); `decode`/`redeem` stay
// `pub(crate)` — the engine's own `resolver`/`params` consume them.
pub use ticket::mint;
