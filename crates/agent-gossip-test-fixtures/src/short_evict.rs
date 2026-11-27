/// Hidden tuning flags that collapse the ~90s eviction window and the heal cadence to seconds.
///
/// Such short cadences destabilise convergence in a real mesh, but a loopback test tolerates them.
pub const SHORT_EVICT: [(&str, &str); 3] = [
    ("--alive-timeout-secs", "3"),
    ("--sweep-interval-secs", "1"),
    ("--heal-interval-secs", "3"),
];
