//! Onion-sealed **setup cells** for telescoping circuit construction.
//!
//! The initiator wraps a per-hop instruction for every hop, innermost-first, so
//! the first hop's layer ends up outermost. Each layer is sealed to that hop's
//! X25519 key (reusing [`crate::protocol::seal`]), so a hop peels only its own
//! layer and learns solely its immediate successor — never the destination or
//! the rest of the path. This is what makes the hops blind (the Tor property).

use anyhow::{Result, anyhow};
use iroh::EndpointId;
use x25519_dalek::StaticSecret;

use crate::protocol::MessageBody;
use crate::protocol::seal::{seal_to_body, unseal_from_body};

/// Identifies a circuit for its lifetime — unique per initiator attempt.
pub(crate) type CircuitId = u64;

/// One hop's setup instruction, before sealing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SetupCell {
    circuit_id: CircuitId,
    /// The next hop to extend to; `None` marks this hop as the terminal (the
    /// destination, which delivers locally instead of forwarding).
    next: Option<EndpointId>,
    /// The onion-sealed setup blob to forward to `next`, opaque to this hop;
    /// `None` at the terminal.
    inner: Option<String>,
}

/// A hop's peeled setup instruction (its own layer removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Peeled {
    pub(crate) circuit_id: CircuitId,
    /// The successor to extend the circuit to, or `None` if we are the terminal.
    pub(crate) next: Option<EndpointId>,
    /// The still-sealed blob to hand the successor, or `None` at the terminal.
    pub(crate) inner: Option<String>,
}

/// Build the onion the initiator sends to its **first** hop. `path` is the
/// ordered hops *after* the initiator, each paired with its X25519 public key;
/// the last entry is the terminal (destination B).
///
/// # Errors
/// Fails on an empty path, a seal failure, or a body-size overflow.
pub(crate) fn build_onion(
    circuit_id: CircuitId,
    path: &[(EndpointId, [u8; 32])],
) -> Result<String> {
    if path.is_empty() {
        return Err(anyhow!("cannot build a circuit with no hops"));
    }
    let mut inner: Option<String> = None;
    // Wrap terminal-first, so the first hop's layer is the outermost one.
    for index in (0..path.len()).rev() {
        let (_, hop_pub) = &path[index];
        let next = path.get(index + 1).map(|(id, _)| *id);
        let cell = SetupCell {
            circuit_id,
            next,
            inner: inner.take(),
        };
        let json = serde_json::to_string(&cell)?;
        let sealed = seal_to_body(hop_pub, &json)?;
        inner = Some(sealed.as_str().to_owned());
    }
    inner.ok_or_else(|| anyhow!("empty circuit path"))
}

/// Peel this hop's layer off a received setup blob with our X25519 secret,
/// revealing our successor (if any) and the blob to forward on.
///
/// # Errors
/// Fails when the blob was not sealed to us (wrong key) or is malformed.
pub(crate) fn peel(blob: &str, our_secret: &StaticSecret) -> Result<Peeled> {
    let body = MessageBody::new(blob.to_owned())?;
    let json = unseal_from_body(our_secret, &body)?;
    let cell: SetupCell = serde_json::from_str(&json)?;
    Ok(Peeled {
        circuit_id: cell.circuit_id,
        next: cell.next,
        inner: cell.inner,
    })
}

#[cfg(test)]
mod tests {
    use iroh::{EndpointId, SecretKey};
    use x25519_dalek::{PublicKey, StaticSecret};

    use super::{build_onion, peel};

    fn eid(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn keypair(seed: u8) -> (StaticSecret, [u8; 32]) {
        let secret = StaticSecret::from([seed; 32]);
        let public = PublicKey::from(&secret).to_bytes();
        (secret, public)
    }

    #[test]
    fn onion_peels_hop_by_hop_revealing_only_the_successor() {
        let (r1_id, r2_id, b_id) = (eid(1), eid(2), eid(3));
        let (r1_sec, r1_pub) = keypair(11);
        let (r2_sec, r2_pub) = keypair(12);
        let (b_sec, b_pub) = keypair(13);
        let path = vec![(r1_id, r1_pub), (r2_id, r2_pub), (b_id, b_pub)];
        let circuit_id = 42;
        let outer = build_onion(circuit_id, &path).expect("build");

        // R1 learns only that its successor is R2.
        let hop1 = peel(&outer, &r1_sec).expect("R1 peels");
        assert_eq!(hop1.circuit_id, circuit_id);
        assert_eq!(hop1.next, Some(r2_id));
        let for_r2 = hop1.inner.expect("more layers");

        // R2 learns only that its successor is B.
        let hop2 = peel(&for_r2, &r2_sec).expect("R2 peels");
        assert_eq!(hop2.next, Some(b_id));
        let for_b = hop2.inner.expect("more layers");

        // B is the terminal: no successor, nothing to forward.
        let hop3 = peel(&for_b, &b_sec).expect("B peels");
        assert_eq!(hop3.next, None);
        assert!(hop3.inner.is_none());
    }

    #[test]
    fn a_hop_cannot_peel_a_layer_sealed_to_another() {
        let path = vec![(eid(1), keypair(11).1)];
        let outer = build_onion(1, &path).expect("build");
        let (wrong_secret, _) = keypair(99);
        assert!(peel(&outer, &wrong_secret).is_err());
    }

    #[test]
    fn empty_path_is_rejected() {
        assert!(build_onion(1, &[]).is_err());
    }
}
