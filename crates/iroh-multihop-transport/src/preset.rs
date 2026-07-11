//! One-call wiring of the multihop transport onto an iroh endpoint builder.
//!
//! Implementing [`Preset`] lets a caller write
//! `Endpoint::builder(presets::N0).preset(handle)` to register the custom
//! transport, its address lookup, and the backup path selector together — the
//! same ergonomic shape iroh's own `test_transport` uses.

use iroh::endpoint::Builder;
use iroh::endpoint::presets::Preset;

use crate::MultihopHandle;

impl Preset for MultihopHandle {
    fn apply(self, builder: Builder) -> Builder {
        builder
            .add_custom_transport(self.custom_transport())
            .address_lookup(self.address_lookup())
            .path_selector(self.path_selector())
    }
}
