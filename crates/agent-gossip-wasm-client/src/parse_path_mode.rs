use habilis_network::net::TransportOpts;
use wasm_bindgen::JsValue;

/// `dynamic` lets a failed ICE negotiation fall back to the relay, but only on
/// a mesh whose id sets `relay_transport` (what `create` mints here). On a
/// lookup-only mesh the pair stays unlinked instead. `webrtc` fails loudly.
/// Named rather than inferred so the caller states the contract it is
/// asserting.
pub(crate) fn parse_path_mode(mode: Option<&str>) -> Result<TransportOpts, JsValue> {
    match mode.map(str::trim).filter(|mode| !mode.is_empty()) {
        None | Some("dynamic") => Ok(TransportOpts::default()),
        Some("webrtc") => Ok(TransportOpts::webrtc_only()),
        Some(other) => Err(JsValue::from_str(&format!(
            "unknown path mode {other:?}; expected `webrtc` or `dynamic`"
        ))),
    }
}
