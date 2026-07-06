//! Wire: the `🎟️` ticket brand + Base58Check(`version ‖ kind ‖ payload`) with a
//! `SHA256d` checksum. The `🎟️` brand is shared with the a2a-bridge and blob
//! tickets and told apart by the `kind` byte; the mesh id's `💬` is a different
//! glyph, so a mesh id fails invite decode on the prefix alone.
//!
//! Payload: `expiry(8, LE i64) ‖ flags(1) ‖ sig(64) ‖ hash_len(2, LE u16) ‖
//! mesh_hash(UTF-8) ‖ root_field`. Bit 0 of `flags` marks a password-protected
//! invite (the root is `seal_symmetric`-wrapped under a password-derived key);
//! otherwise `root_field` is the 32-byte root in the clear. The issuer signs the
//! *plaintext* root, so a redeemer decrypts first, then verifies.

use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use iroh::{PublicKey, SecretKey, Signature};
use sha2::{Digest, Sha256};

use crate::protocol::crypto::{self, Password};
use crate::protocol::identity;
use crate::protocol::seal;
use crate::protocol::mesh::Mesh;
use crate::util::clock;
use crate::util::consts::{MESH_URI_SEPARATOR, TICKET_GLYPH};

/// Branding prefix on every ticket — the `🎟️` ticket glyph; the remainder of
/// the string is ASCII Base58Check. Shared with the a2a and blob tickets and
/// told apart by [`KIND`]; the mesh id's `💬` is a different glyph entirely.
const PREFIX: &str = TICKET_GLYPH;

/// Framing version. Bumped only on a breaking framing change.
const VERSION: u8 = 1;

/// Ticket-kind discriminant. Distinct from the a2a bridge ticket (`2`) and the
/// blob ticket (`1`) so a wrong-kind token is rejected on decode.
const KIND: u8 = 3;

/// Bit 0 of the flags byte: the password flag.
const PASSWORD_BIT: u8 = 0b0000_0001;

/// Domain-separation prefix on the issuer-signed bytes, so an invite signature
/// can never be replayed as any other Ed25519 signature in the system.
const SIG_DOMAIN: &[u8] = b"agent-mesh/invite/v1";

/// A decoded invite ticket. Parsing (`decode`) is structural only — verifying
/// the issuer signature, checking expiry, and unwrapping the root all happen in
/// [`Self::redeem`], which needs the password for a protected invite.
#[derive(Clone, PartialEq, Eq)]
pub struct InviteTicket {
    mesh_hash: String,
    /// Unix seconds after which the invite is refused, or `0` for no expiry.
    expiry: i64,
    password: bool,
    sig: [u8; 64],
    /// The 32-byte root in the clear, or its `seal_symmetric` blob when
    /// password-protected.
    root_field: Vec<u8>,
}

impl fmt::Debug for InviteTicket {
    // Redact the root — for a passwordless invite it is the mesh's join key in
    // the clear, so a stray `{:?}` must never leak it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InviteTicket")
            .field("mesh_hash", &self.mesh_hash)
            .field("expiry", &self.expiry)
            .field("password", &self.password)
            .field("root_field", &"***")
            .finish_non_exhaustive()
    }
}

/// The exact bytes the issuer signs / a redeemer verifies: a domain tag, the
/// published mesh hash, the plaintext root, and the expiry.
fn signing_bytes(mesh_hash: &str, root: &[u8; 32], expiry: i64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SIG_DOMAIN.len() + mesh_hash.len() + 32 + 8);
    bytes.extend_from_slice(SIG_DOMAIN);
    bytes.extend_from_slice(mesh_hash.as_bytes());
    bytes.extend_from_slice(root);
    bytes.extend_from_slice(&expiry.to_le_bytes());
    bytes
}

/// Mint a `🎟️` invite for `mesh`. `ttl_secs` `None`/`Some(0)` ⇒ no expiry,
/// else the invite expires `ttl_secs` from now. `password` (the mesh's, if it
/// has one) wraps the root so a scraped invite still needs it.
///
/// # Errors
/// This session is not the creator (holds no issuer key), or `mesh` is not
/// invite-only.
pub fn mint(mesh: &Mesh, ttl_secs: Option<u64>, password: Option<&Password>) -> Result<String> {
    let issuer_secret = mesh
        .issuer_secret()
        .context("only the creator can mint invites (this session holds no issuer key)")?;
    let root = *mesh.invite_key().context("not an invite-only mesh")?;
    let mesh_hash = mesh.to_string();
    let expiry = match ttl_secs {
        None | Some(0) => 0,
        Some(ttl) => clock::unix_secs().saturating_add(i64::try_from(ttl).unwrap_or(i64::MAX)),
    };
    let signature =
        SecretKey::from_bytes(issuer_secret).sign(&signing_bytes(&mesh_hash, &root, expiry));
    let root_field = match password {
        Some(password) => {
            let wrap_key = crypto::invite_wrap_key(password, mesh.wrap_salt());
            seal::seal_symmetric(&wrap_key, &root)
        }
        None => root.to_vec(),
    };
    let ticket = InviteTicket {
        mesh_hash,
        expiry,
        password: password.is_some(),
        sig: signature.to_bytes(),
        root_field,
    };
    Ok(ticket.encode())
}

impl InviteTicket {
    /// Whether this invite is password-protected (the redeemer must supply the
    /// mesh password to unwrap the root).
    #[must_use]
    pub fn requires_password(&self) -> bool {
        self.password
    }

    fn encode(&self) -> String {
        let hash_bytes = self.mesh_hash.as_bytes();
        let hash_len = u16::try_from(hash_bytes.len()).expect("mesh hash fits in a u16 length");
        let mut payload =
            Vec::with_capacity(8 + 1 + 64 + 2 + hash_bytes.len() + self.root_field.len());
        payload.extend_from_slice(&self.expiry.to_le_bytes());
        payload.push(if self.password { PASSWORD_BIT } else { 0 });
        payload.extend_from_slice(&self.sig);
        payload.extend_from_slice(&hash_len.to_le_bytes());
        payload.extend_from_slice(hash_bytes);
        payload.extend_from_slice(&self.root_field);
        let mut framed = Vec::with_capacity(2 + payload.len());
        framed.push(VERSION);
        framed.push(KIND);
        framed.extend_from_slice(&payload);
        format!(
            "{PREFIX}{MESH_URI_SEPARATOR}{}",
            base58check_encode(&framed)
        )
    }

    /// Decode a `🎟️` invite ticket — structural parse only.
    ///
    /// # Errors
    /// Not a `🎟️` token, the wrong kind, a bad checksum/version, or a malformed
    /// payload.
    pub(crate) fn decode(token: &str) -> Result<Self> {
        let body = strip_ticket_prefix(token.trim())
            .with_context(|| format!("not an invite ticket: must start with {PREFIX}"))?;
        let framed = base58check_decode(body)?;
        let version = *framed.first().context("ticket too short")?;
        if version != VERSION {
            bail!("unsupported invite ticket version: {version}");
        }
        let kind = *framed.get(1).context("ticket too short")?;
        if kind != KIND {
            bail!("not an invite ticket: wrong ticket kind");
        }
        let payload = &framed[2..];
        let mut pos = 0;
        let expiry =
            i64::from_le_bytes(take_array::<8>(payload, &mut pos).context("missing expiry")?);
        let flags = *payload.get(pos).context("missing flags")?;
        pos += 1;
        let password = flags & PASSWORD_BIT != 0;
        let sig = take_array::<64>(payload, &mut pos).context("missing signature")?;
        let hash_len = usize::from(u16::from_le_bytes(
            take_array::<2>(payload, &mut pos).context("missing hash length")?,
        ));
        let hash_end = pos.checked_add(hash_len).context("hash length overflow")?;
        let hash_bytes = payload.get(pos..hash_end).context("truncated mesh hash")?;
        pos = hash_end;
        let mesh_hash = std::str::from_utf8(hash_bytes)
            .context("invalid mesh hash utf-8")?
            .to_owned();
        let root_field = payload.get(pos..).context("missing root")?.to_vec();
        Ok(Self {
            mesh_hash,
            expiry,
            password,
            sig,
            root_field,
        })
    }

    /// Redeem the invite: reconstruct the mesh, unwrap the root (needs the
    /// password iff protected), verify the creator's signature, and reject an
    /// expired invite. Returns a mesh ready to join.
    ///
    /// # Errors
    /// A malformed embedded hash, a missing/wrong password, an invalid issuer
    /// signature (not creator-minted or tampered), or an expired invite.
    pub(crate) fn redeem(&self, password: Option<&Password>) -> Result<Mesh> {
        let mut mesh: Mesh = self
            .mesh_hash
            .parse()
            .context("invite carries an invalid mesh id")?;
        let issuer_pubkey = *mesh
            .issuer_pubkey()
            .context("invite is for a non-invite-only mesh")?;
        let root = self.unwrap_root(&mesh, password)?;
        let pubkey = PublicKey::from_bytes(&issuer_pubkey).context("invalid issuer public key")?;
        let signature = Signature::from_bytes(&self.sig);
        if !identity::verify(
            &pubkey,
            &signing_bytes(&self.mesh_hash, &root, self.expiry),
            &signature,
        ) {
            bail!("invalid invite signature — not creator-minted, or tampered");
        }
        if self.expiry != 0 && clock::unix_secs() >= self.expiry {
            bail!("this invite has expired");
        }
        mesh.apply_invite(root);
        Ok(mesh)
    }

    fn unwrap_root(&self, mesh: &Mesh, password: Option<&Password>) -> Result<[u8; 32]> {
        let raw = match (self.password, password) {
            (true, Some(password)) => {
                let wrap_key = crypto::invite_wrap_key(password, mesh.wrap_salt());
                seal::open_symmetric(&wrap_key, &self.root_field)
                    .context("wrong password or corrupt invite")?
            }
            (true, None) => bail!("this invite is password-protected — pass --password"),
            (false, Some(_)) => bail!("this invite has no password — drop --password"),
            (false, None) => self.root_field.clone(),
        };
        raw.try_into()
            .map_err(|_| anyhow!("invite root is not 32 bytes"))
    }
}

/// Strip the ticket brand and optional `://` off a token, returning the
/// Base58Check body. Accepts the canonical `🎟️://` and a paste that dropped the
/// VS-16 (`🎟://`) or the separator. `None` if the token lacks the ticket glyph.
fn strip_ticket_prefix(token: &str) -> Option<&str> {
    let base = TICKET_GLYPH
        .strip_suffix('\u{FE0F}')
        .unwrap_or(TICKET_GLYPH);
    let rest = token.strip_prefix(base)?;
    let rest = rest.strip_prefix('\u{FE0F}').unwrap_or(rest);
    Some(rest.strip_prefix(MESH_URI_SEPARATOR).unwrap_or(rest))
}

/// Read `N` bytes at `*pos` into a fixed array, advancing `*pos`. `None` if the
/// slice is too short.
fn take_array<const N: usize>(bytes: &[u8], pos: &mut usize) -> Option<[u8; N]> {
    let slice = bytes.get(*pos..*pos + N)?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    *pos += N;
    Some(out)
}

fn checksum(bytes: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(bytes);
    let second = Sha256::digest(first);
    let mut out = [0u8; 4];
    out.copy_from_slice(&second[..4]);
    out
}

fn base58check_encode(payload: &[u8]) -> String {
    let mut with_checksum = payload.to_vec();
    with_checksum.extend_from_slice(&checksum(payload));
    bs58::encode(with_checksum).into_string()
}

fn base58check_decode(encoded: &str) -> Result<Vec<u8>> {
    let decoded = bs58::decode(encoded)
        .into_vec()
        .context("invalid Base58 in invite ticket")?;
    if decoded.len() < 4 {
        bail!("invite ticket too short");
    }
    let (payload, received) = decoded.split_at(decoded.len() - 4);
    if received != checksum(payload) {
        bail!("invalid invite ticket checksum");
    }
    Ok(payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{InviteTicket, mint, signing_bytes};
    use crate::protocol::crypto::Password;
    use crate::protocol::mesh::{Mesh, MeshConfig, MeshName};
    use iroh::SecretKey;

    fn pw(text: &str) -> Password {
        Password::new(text.to_owned())
    }

    /// A fresh invite-only mesh on the creator (holds the issuer key + root).
    fn creator() -> Mesh {
        let mut mesh = Mesh::new(
            [1u8; 32],
            MeshName::new("t").unwrap(),
            MeshConfig::loopback(),
        );
        mesh.set_invite();
        assert!(
            mesh.issuer_secret().is_some(),
            "the creator holds the issuer key"
        );
        mesh
    }

    fn minted(mesh: &Mesh, ttl: Option<u64>, password: Option<&Password>) -> InviteTicket {
        InviteTicket::decode(&mint(mesh, ttl, password).unwrap()).expect("decode")
    }

    // ── happy path ────────────────────────────────────────────────────────

    #[test]
    fn redeemed_invite_derives_the_creators_topic() {
        let mesh = creator();
        let redeemed = minted(&mesh, Some(3600), None).redeem(None).unwrap();
        assert!(redeemed.requires_invite());
        // The gate works: same root ⇒ same topic ⇒ they actually meet.
        assert_eq!(redeemed.topic_id(), mesh.topic_id());
    }

    #[test]
    fn no_expiry_invite_round_trips() {
        let mesh = creator();
        for ttl in [None, Some(0)] {
            let ticket = minted(&mesh, ttl, None);
            assert!(ticket.redeem(None).is_ok(), "0/none ttl never expires");
        }
    }

    // ── "only the creator can mint" ───────────────────────────────────────

    #[test]
    fn a_redeemer_holds_the_root_but_cannot_mint() {
        let mesh = creator();
        let redeemed = minted(&mesh, Some(3600), None).redeem(None).unwrap();
        // The redeemer has the join root (it derived the topic) but no issuer
        // key, so it can neither mint nor sign a forgery.
        assert!(redeemed.issuer_secret().is_none());
        assert!(
            mint(&redeemed, Some(3600), None).is_err(),
            "a non-creator must not be able to mint a valid invite"
        );
    }

    #[test]
    fn mint_refuses_a_non_invite_only_mesh() {
        let plain = Mesh::new(
            [2u8; 32],
            MeshName::new("t").unwrap(),
            MeshConfig::loopback(),
        );
        assert!(mint(&plain, Some(3600), None).is_err());
    }

    // ── adversarial: forged / tampered signatures ─────────────────────────

    #[test]
    fn rejects_a_zeroed_or_garbage_signature() {
        let mesh = creator();
        let mut ticket = minted(&mesh, Some(3600), None);
        ticket.sig = [0u8; 64];
        assert!(ticket.redeem(None).is_err(), "a blank sig must be refused");
    }

    #[test]
    fn rejects_an_invite_signed_by_a_non_issuer_key() {
        // An attacker who knows the mesh hash + root (a member) signs an
        // otherwise-valid invite with their OWN key. It must fail against the
        // issuer pubkey baked into the hash — this is what makes minting
        // creator-only rather than any-member.
        let mesh = creator();
        let hash = mesh.to_string();
        let root = *mesh.invite_key().unwrap();
        let expiry = crate::util::clock::unix_secs() + 3600;
        let attacker = SecretKey::from_bytes(&[9u8; 32]);
        let forged = InviteTicket {
            mesh_hash: hash.clone(),
            expiry,
            password: false,
            sig: attacker
                .sign(&signing_bytes(&hash, &root, expiry))
                .to_bytes(),
            root_field: root.to_vec(),
        };
        assert!(
            forged.redeem(None).is_err(),
            "an invite signed by a non-issuer must be refused"
        );
    }

    #[test]
    fn rejects_a_tampered_expiry() {
        // Extending the window by editing `expiry` invalidates the signature,
        // which covers it — so a bearer can't lengthen their own invite.
        let mesh = creator();
        let mut ticket = minted(&mesh, Some(3600), None);
        ticket.expiry = i64::MAX;
        assert!(ticket.redeem(None).is_err());
    }

    #[test]
    fn rejects_a_tampered_root() {
        let mesh = creator();
        let mut ticket = minted(&mesh, Some(3600), None);
        ticket.root_field[0] ^= 0xff;
        assert!(
            ticket.redeem(None).is_err(),
            "a swapped root breaks the signature over the plaintext root"
        );
    }

    // ── adversarial: expiry ───────────────────────────────────────────────

    #[test]
    fn rejects_an_expired_invite_even_with_a_valid_signature() {
        // A genuinely creator-signed invite whose window has passed: the
        // signature is valid, but the local-clock check still refuses it.
        let mesh = creator();
        let hash = mesh.to_string();
        let root = *mesh.invite_key().unwrap();
        let past = crate::util::clock::unix_secs() - 100;
        let issuer = SecretKey::from_bytes(mesh.issuer_secret().unwrap());
        let expired = InviteTicket {
            mesh_hash: hash.clone(),
            expiry: past,
            password: false,
            sig: issuer.sign(&signing_bytes(&hash, &root, past)).to_bytes(),
            root_field: root.to_vec(),
        };
        let error = expired.redeem(None).unwrap_err().to_string();
        assert!(error.contains("expired"), "got: {error}");
    }

    // ── adversarial: password wrap ────────────────────────────────────────

    #[test]
    fn a_password_wrapped_invite_needs_the_right_password() {
        let mesh = creator();
        let ticket = minted(&mesh, Some(3600), Some(&pw("hunter2")));
        assert!(ticket.requires_password());
        assert!(ticket.redeem(None).is_err(), "missing password");
        assert!(
            ticket.redeem(Some(&pw("wrong"))).is_err(),
            "a wrong password must not unwrap the root (AEAD self-verifies)"
        );
        let ok = ticket.redeem(Some(&pw("hunter2"))).unwrap();
        assert_eq!(ok.topic_id(), mesh.topic_id());
    }

    #[test]
    fn a_plain_invite_rejects_a_stray_password() {
        let mesh = creator();
        assert!(
            minted(&mesh, Some(3600), None)
                .redeem(Some(&pw("x")))
                .is_err()
        );
    }

    // ── adversarial: cross-token confusion ────────────────────────────────

    #[test]
    fn a_mesh_id_is_not_an_invite() {
        // A plain `💬…` id carries the mesh glyph, not the ticket `🎟️`.
        let plain = Mesh::new(
            [3u8; 32],
            MeshName::new("t").unwrap(),
            MeshConfig::loopback(),
        );
        assert!(InviteTicket::decode(&plain.to_string()).is_err());
    }

    #[test]
    fn an_invite_and_a_blob_ticket_never_cross_parse() {
        use crate::blob::BlobTicket;
        let addr = {
            let id = SecretKey::from_bytes(&[4u8; 32]).public();
            iroh::EndpointAddr::new(id).with_ip_addr("127.0.0.1:4242".parse().unwrap())
        };
        let blob = BlobTicket {
            addr,
            secret: [9u8; crate::blob::SECRET_LEN],
            sha256: [7u8; crate::blob::HASH_LEN],
            size: 1,
            lookups: crate::protocol::mesh::LookupOpts::loopback(),
            password: false,
        }
        .encode();
        let invite = mint(&creator(), Some(3600), None).unwrap();
        // Same `🎟️` brand, different kind byte — each rejects the other.
        assert!(InviteTicket::decode(&blob).is_err());
        assert!(BlobTicket::decode(&invite).is_err());
    }

    #[test]
    fn rejects_a_bad_checksum() {
        let mut token = mint(&creator(), Some(3600), None).unwrap();
        let last = token.pop().unwrap();
        token.push(if last == '1' { '2' } else { '1' });
        assert!(InviteTicket::decode(&token).is_err());
    }

    #[test]
    fn decode_tolerates_missing_variation_selector() {
        let token = mint(&creator(), Some(3600), None).unwrap();
        let bare = token.replacen('\u{FE0F}', "", 1);
        assert_ne!(bare, token);
        assert!(InviteTicket::decode(&bare).is_ok());
    }
}
