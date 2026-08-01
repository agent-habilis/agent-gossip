//! What an idle daemon pays for one `AF_ROUTE` message.
//!
//! iroh stands up a `netwatch` netmon per endpoint. On macOS that monitor
//! subscribes to the `AF_ROUTE` socket and, 250ms after any message it deems
//! interesting, rebuilds `interfaces::State` — `netdev::get_interfaces()`,
//! which on this platform means `getifaddrs` plus an `if_nametoindex` per
//! address (itself another `getifaddrs`) plus a synchronous `CoreWLAN` XPC
//! round-trip per Wi-Fi interface. None of that is our code, but all of it is
//! our idle CPU.
//!
//! Unlike `hot_paths`, this bench is deliberately impure: it measures host
//! syscalls, so the result is a property of the machine — dominated by how many
//! interfaces it has — and is only comparable against a run that reports the
//! same interface count. It prints that count before the table for exactly that
//! reason. A laptop on a VPN with a dozen `utun`s pays several times what a
//! bare host does.
//!
//! Run: `cargo bench -p agent-gossip --bench idle_cost`.

use divan::{Bencher, black_box};
use netwatch::interfaces::{State, default_route_interface};

fn main() {
    let probe = runtime().block_on(State::new());
    println!(
        "host: {} interfaces, {} local addresses\n",
        probe.interfaces.len(),
        probe.local_addresses.regular.len() + probe.local_addresses.loopback.len(),
    );
    divan::main();
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
}

/// The whole cost of one debounced `AF_ROUTE` wakeup: what
/// `netmon::Actor::handle_potential_change` runs before it compares the result
/// against the previous state and (on an idle host, always) discards it.
#[divan::bench]
fn netmon_state_new(bencher: Bencher<'_, '_>) {
    let rt = runtime();
    bencher.bench_local(|| black_box(rt.block_on(State::new())));
}

/// The *second* enumeration inside that same `State::new()`. It fetches the
/// interface list a second time purely to name the default route, having
/// already had it in hand. Subtracting this from `netmon_state_new` prices the
/// duplicate — i.e. the ceiling on what de-duplicating it can save.
#[divan::bench]
fn default_route_only(bencher: Bencher<'_, '_>) {
    let rt = runtime();
    bencher.bench_local(|| black_box(rt.block_on(default_route_interface())));
}
