// Cargo-style status output, shared verbatim with the `ah-s` binary: this
// module `include!`s the canonical source at `../src/util/output.rs`, so both
// surfaces print identically with no crate dependency. The dead-code expect for
// the subset this crate uses lives on the `mod output` declaration in `util`.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../src/util/output.rs"
));
