/// Byte-domain for this bridge's ticket tokens, passed to
/// `TicketAuth::derive`. It is stretched into the token and so reaches the wire:
/// fixed for the life of the ticket kind, and distinct from every other kind's
/// (the engine's blob tickets use `b"blob-ticket"`) so a shared secret never
/// derives the same passworded token twice.
pub(crate) const A2A_TICKET_LABEL: &[u8] = b"a2a-ticket";
