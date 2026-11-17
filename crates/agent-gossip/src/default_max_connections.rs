/// How many direct connections a native node holds by default, gossip
/// neighbors included. A browser tab holds 8. The app owns this number, so it
/// does not move with the engine rev it is built against.
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;
