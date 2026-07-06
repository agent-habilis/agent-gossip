// Cargo-style status output, shared verbatim with the `agent-mesh` engine: this
// module `include!`s the canonical source at
// `../agent-habilis-mesh/src/util/output.rs`, so both surfaces print
// identically with no crate dependency. The dead-code expect for the subset this
// crate uses lives on the `mod output` declaration in `util`.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../agent-habilis-mesh/src/util/output.rs"
));
