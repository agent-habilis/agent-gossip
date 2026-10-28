/// Whether this host can send to the mDNS multicast groups at all — the
/// precondition for any test whose peers find each other over LAN multicast.
///
/// Deliberately asymmetric: a successful send does **not** prove discovery
/// works (nothing may be listening), but a failure on *both* families proves it
/// cannot, so this only ever gates out a host that is certainly incapable. A
/// VPN that captures the default route, or a runner image without an `IPv6`
/// route, makes every `send_to` fail with `EHOSTUNREACH` — and an mDNS test
/// then burns its whole budget proving only that the host has no multicast.
pub fn mdns_multicast_available() -> bool {
    fn can_send(bind: &str, group: &str) -> bool {
        std::net::UdpSocket::bind(bind).is_ok_and(|sock| sock.send_to(&[0_u8], group).is_ok())
    }
    can_send("0.0.0.0:0", "224.0.0.251:5353") || can_send("[::]:0", "[ff02::fb]:5353")
}
