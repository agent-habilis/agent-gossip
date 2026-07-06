//! The mesh identifier, at two levels:
//!
//! - [`MeshId`] — the validated `💬…` *string* (shallow: prefix +
//!   length + Base58 charset). Cheap boundary check at the CLI / IPC
//!   edge. Code: [`id`].
//! - [`Mesh`] — the *decoded* structure (32-byte seed + name +
//!   [`MeshConfig`]), with the Base58Check codec (this file). `MeshId`
//!   is what flows through the wire/CLI; `Mesh` is what `setup_mesh`
//!   derives identity and behavior from.
//!
//! See `docs/mesh-hash.md` for the full byte layout and rationale.
//!
//! Also home to [`MeshName`] ([`name`]), the lookup allowlist ([`lookup`])
//! carried in the id, and the relay-ladder parsing.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh::{EndpointId, SecretKey};
use rand::RngCore;
use sha2::{Digest, Sha256};

use super::crypto;

mod id;
mod lookup;
mod name;

pub use id::{MeshId, MeshIdError};
pub use lookup::{
    AdvertiseRequiresReachable, DEFAULT_DIRECTORY, DirectorySelection, LookupOpts, RelayChoice,
    MeshConfig, resolve_lookups, validate_advertise,
};
pub use lookup::{LookupSet, RelayLadder, RelayLadderError, RelaySelection};
pub use name::{NameError, MeshName};

const PREFIX: &str = crate::util::consts::MESH_GLYPH;

/// The URI separator that follows the `💬` sigil in the canonical id
/// (`💬://<base58>`). Optional on input — a legacy bare `💬<base58>` id
/// still parses. Never appears in a filesystem path (see
/// [`crate::util::mesh_prefix`]).
pub(crate) const SEPARATOR: &str = crate::util::consts::MESH_URI_SEPARATOR;

/// Id format version. A single byte reserved so the encoding can evolve;
/// an unknown version is rejected.
const VERSION: u8 = 1;
const SEED_LEN: usize = 32;
/// Wire bound for the encoded name in bytes. `MeshName::new` caps the
/// name at `ident::MAX_CHARS` scalar values; each is at most 4 UTF-8
/// bytes, so the encoded form fits this many bytes (and inside the
/// 1-byte length field).
const NAME_MAX_BYTES: usize = super::ident::MAX_CHARS * 4;

/// A mesh identifier — Base58Check payload with a `💬` prefix.
///
/// The token carries the random `seed` plus the mesh's [`MeshConfig`]
/// (lookups); **no peer address is ever stored**. The
/// gossip topic, the well-known rendezvous identity (every joiner's
/// bootstrap target), and the loopback port ladder are all derived from
/// `seed` in memory, so the mesh is creator-independent and survives
/// the creator's death. The config is mixed into the topic derivation,
/// so every member of a mesh shares the same config.
///
/// Wire format (little-endian):
///
/// ```text
/// [1 byte version]
/// [32 bytes seed]
/// [1 byte name length in bytes, 1..=128]
/// [N bytes name (UTF-8, <=32 scalars, charset enforced by MeshName)]
/// [2 bytes config length] [config bytes (see MeshConfig::to_bytes)]
/// ```
#[derive(Clone)]
pub struct Mesh {
    pub name: MeshName,
    seed: [u8; SEED_LEN],
    /// The Argon2id-stretched password key, once a password is applied.
    /// Every derivation (topic, rendezvous, port ladder) switches onto it,
    /// so holding the id without the password computes nothing reachable.
    /// Never serialized — `encode_bytes` writes `seed`.
    stretched_key: Option<[u8; SEED_LEN]>,
    /// The invite-only derivation secret (the "root"), held only in memory:
    /// baked into no hash, carried only by creator-minted invites. When set,
    /// every derivation switches onto it (see [`Self::effective_seed`]), so the
    /// bare invite-only hash reaches nothing. Set on the creator by
    /// [`Self::set_invite`] and on a redeemer by [`Self::apply_invite`].
    invite_key: Option<[u8; SEED_LEN]>,
    /// The Ed25519 issuer **private** key of an invite-only mesh — the mint
    /// authority, held only by the creator's live session (in-memory only, so a
    /// restart ends minting). `None` on every non-creator. The matching public
    /// key rides the hash (`config.issuer_pubkey`).
    issuer_secret: Option<[u8; 32]>,
    pub config: MeshConfig,
}

impl fmt::Debug for Mesh {
    // Redact all secret material — the seed *is* the join capability, and the
    // invite root / issuer secret gate an invite-only mesh — so a stray `{:?}`
    // in a log can never leak them (mirrors `crypto::Password`).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Mesh")
            .field("name", &self.name)
            .field("seed", &"***")
            .field("stretched_key", &self.stretched_key.map(|_| "***"))
            .field("invite_key", &self.invite_key.map(|_| "***"))
            .field("issuer_secret", &self.issuer_secret.map(|_| "***"))
            .field("config", &self.config)
            .finish()
    }
}

impl Mesh {
    #[must_use]
    pub fn new(seed: [u8; SEED_LEN], name: MeshName, config: MeshConfig) -> Self {
        Mesh {
            name,
            seed,
            stretched_key: None,
            invite_key: None,
            issuer_secret: None,
            config,
        }
    }

    /// Build a mesh deterministically from an arbitrary string (the `topic`
    /// command). The seed is [`crypto::topic_seed`] of the string; the name is
    /// the string itself sanitized into a [`MeshName`]
    /// ([`MeshName::from_topic_string`]). Both are deterministic functions of
    /// the string, so callers passing the same string + config converge on the
    /// same id with zero coordination.
    #[must_use]
    pub fn from_topic(topic: &str, config: MeshConfig) -> Self {
        let seed = crypto::topic_seed(topic);
        let name = MeshName::from_topic_string(topic);
        Mesh {
            name,
            seed,
            stretched_key: None,
            invite_key: None,
            issuer_secret: None,
            config,
        }
    }

    /// The wire seed (what the encoded id carries). Test-only: production
    /// derivations go through [`Mesh::effective_seed`] so the password mix
    /// can never be bypassed.
    #[cfg(test)]
    pub(crate) fn seed(&self) -> &[u8; SEED_LEN] {
        &self.seed
    }

    /// The seed every derivation uses: the invite root for an invite-only
    /// mesh, the stretched password key when a password is applied, else the
    /// wire seed. Asserts the secret is present before deriving — deriving from
    /// the raw seed would silently land in an unreachable topic, strictly worse
    /// than failing loudly. (Invite-only and password are mutually exclusive in
    /// the id: an invite-only mesh's password, if any, only wraps its invite
    /// tickets — it never folds into the topic here.)
    fn effective_seed(&self) -> &[u8; SEED_LEN] {
        if self.requires_invite() {
            return self
                .invite_key
                .as_ref()
                .expect("invite-only mesh derived before an invite was applied");
        }
        if let Some(key) = &self.stretched_key {
            return key;
        }
        assert!(
            self.config.password.is_none(),
            "passworded mesh derived before the password was applied"
        );
        &self.seed
    }

    /// Whether the id carries a password verifier (joiners must present
    /// the password).
    #[must_use]
    pub fn requires_password(&self) -> bool {
        self.config.password.is_some()
    }

    /// The Argon2id-stretched password key, once a password has been applied
    /// (`set_password`/`apply_password`) — `None` for a passwordless mesh. The
    /// daemon retains this to key the state/meta channel encryption; it is the
    /// same value every derivation switches onto ([`Self::effective_seed`]).
    pub(crate) fn stretched_key(&self) -> Option<[u8; SEED_LEN]> {
        self.stretched_key
    }

    /// Join-side: stretch `password` (salt = the wire seed), check it
    /// against the verifier the id carries, and switch every derivation
    /// onto the stretched key. ~100ms of Argon2id by design.
    ///
    /// # Errors
    /// The id carries no verifier (passwordless mesh), or the password is
    /// wrong.
    pub(crate) fn apply_password(&mut self, password: &crypto::Password) -> Result<()> {
        let Some(expected) = &self.config.password else {
            bail!("this mesh has no password — drop --password");
        };
        let key = crypto::stretch_mesh_password(password, &self.seed);
        if crypto::password_verifier(&key) != *expected {
            bail!("wrong password");
        }
        self.stretched_key = Some(key);
        Ok(())
    }

    /// Create-side: stretch `password`, bake its verifier into the config
    /// (the encoded id must carry it), and switch every derivation onto
    /// the stretched key. ~100ms of Argon2id by design.
    pub(crate) fn set_password(&mut self, password: &crypto::Password) {
        let key = crypto::stretch_mesh_password(password, &self.seed);
        self.config.password = Some(crypto::password_verifier(&key));
        self.stretched_key = Some(key);
    }

    /// Whether this mesh is invite-only (the id carries an issuer pubkey, so
    /// the bare hash cannot join — a creator-minted invite is required).
    #[must_use]
    pub fn requires_invite(&self) -> bool {
        self.config.issuer_pubkey.is_some()
    }

    /// Create-side: turn this into an invite-only mesh. Mints a random invite
    /// **root** (the derivation secret, held only in memory and in the invites
    /// this creator issues) and an Ed25519 **issuer** keypair (the mint
    /// authority), baking the issuer public key into the id. Both secrets stay
    /// in-memory only, so a creator restart ends minting.
    pub fn set_invite(&mut self) {
        let mut root = [0u8; SEED_LEN];
        rand::rng().fill_bytes(&mut root);
        let mut issuer = [0u8; 32];
        rand::rng().fill_bytes(&mut issuer);
        let issuer_pubkey = SecretKey::from_bytes(&issuer).public();
        self.config.issuer_pubkey = Some(*issuer_pubkey.as_bytes());
        self.invite_key = Some(root);
        self.issuer_secret = Some(issuer);
    }

    /// Redeem-side: switch every derivation onto the invite `root` carried by a
    /// verified invite ticket. The redeemer holds no issuer secret, so it can
    /// join but never mint.
    pub(crate) fn apply_invite(&mut self, root: [u8; SEED_LEN]) {
        self.invite_key = Some(root);
    }

    /// The issuer private key, for signing a minted invite (creator only).
    pub(crate) fn issuer_secret(&self) -> Option<&[u8; 32]> {
        self.issuer_secret.as_ref()
    }

    /// The issuer public key the id carries — a redeemer verifies each invite's
    /// signature against it.
    pub(crate) fn issuer_pubkey(&self) -> Option<&[u8; 32]> {
        self.config.issuer_pubkey.as_ref()
    }

    /// The invite root secret to embed in a minted invite (creator only).
    pub(crate) fn invite_key(&self) -> Option<&[u8; SEED_LEN]> {
        self.invite_key.as_ref()
    }

    /// The public identity salt (the wire seed) an invite ticket wraps its root
    /// under when password-protected — public and in the hash, so both minter
    /// and redeemer derive the same wrap key from the password alone.
    pub(crate) fn wrap_salt(&self) -> &[u8; SEED_LEN] {
        &self.seed
    }

    /// The gossip `TopicId` — the one derivation entry point, so no caller
    /// can forget the password mix.
    pub(crate) fn topic_id(&self) -> iroh_gossip::proto::TopicId {
        crypto::derive_topic_id(self.effective_seed(), &self.name, &self.config_bytes())
    }

    /// The shared rendezvous endpoint secret every co-host binds.
    pub(crate) fn rendezvous_secret(&self) -> SecretKey {
        crypto::rendezvous_secret(self.effective_seed())
    }

    #[must_use]
    pub fn lookups(&self) -> &LookupOpts {
        &self.config.lookups
    }

    /// True when the mesh is loopback-only (no off-machine lookups).
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.config.lookups.is_loopback()
    }

    /// `"private"`/`"public"` label for output, derived from the lookups.
    #[must_use]
    pub fn network_label(&self) -> &'static str {
        self.config.lookups.network_label()
    }

    /// The canonical config bytes mixed into the topic derivation (so a
    /// mesh's config is part of its identity).
    pub(crate) fn config_bytes(&self) -> Vec<u8> {
        self.config.to_bytes()
    }

    /// Well-known rendezvous `EndpointId`, derived from the effective seed.
    /// Every joiner computes this locally and bootstraps gossip from it; it
    /// is co-hosted by members rather than pinned to the creator.
    #[must_use]
    pub fn rendezvous_id(&self) -> EndpointId {
        crypto::rendezvous_id(self.effective_seed())
    }

    /// Deterministic loopback port *ladder* for loopback-only meshes (no
    /// pkarr/DNS to resolve `rendezvous_id`). Preference order; a beacon
    /// binds the first free rung, joiners try all rungs.
    #[must_use]
    pub fn rendezvous_ports(&self) -> [u16; crypto::RENDEZVOUS_LADDER] {
        crypto::rendezvous_ports(self.effective_seed())
    }

    fn encode_bytes(&self) -> Vec<u8> {
        let config = self.config.to_bytes();
        let mut buf =
            Vec::with_capacity(1 + SEED_LEN + 1 + self.name.as_bytes().len() + 2 + config.len());
        buf.push(VERSION);
        buf.extend_from_slice(&self.seed);
        // MeshName guarantees 1..=128 UTF-8 bytes, so a 1-byte length is safe.
        buf.push(self.name.len_u8());
        buf.extend_from_slice(self.name.as_bytes());
        let config_len =
            u16::try_from(config.len()).expect("MeshConfig encodes well within a u16 length");
        buf.extend_from_slice(&config_len.to_le_bytes());
        buf.extend_from_slice(&config);
        buf
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self> {
        let mut pos = 0usize;
        let version = *bytes.get(pos).context("Mesh identifier too short")?;
        pos += 1;
        if version != VERSION {
            bail!("Unsupported mesh id version: {version}");
        }

        let seed_slice = bytes
            .get(pos..pos + SEED_LEN)
            .context("Mesh identifier too short")?;
        let mut seed = [0u8; SEED_LEN];
        seed.copy_from_slice(seed_slice);
        pos += SEED_LEN;

        let name_len = *bytes.get(pos).context("Truncated mesh name length")? as usize;
        pos += 1;
        if name_len == 0 || name_len > NAME_MAX_BYTES {
            bail!("Invalid mesh name length: {name_len}");
        }
        let name_raw = bytes
            .get(pos..pos + name_len)
            .context("Truncated mesh name")?;
        pos += name_len;
        let name_str = std::str::from_utf8(name_raw).context("Invalid mesh name UTF-8")?;
        let name = MeshName::new(name_str).context("Invalid mesh name")?;

        let config_len =
            lookup::read_u16(bytes, &mut pos).context("Truncated config length")? as usize;
        let config_raw = bytes
            .get(pos..pos + config_len)
            .context("Truncated mesh config")?;
        pos += config_len;
        if pos != bytes.len() {
            bail!("Trailing bytes in mesh identifier");
        }
        let config = MeshConfig::from_bytes(config_raw)?;

        Ok(Mesh {
            name,
            seed,
            stretched_key: None,
            invite_key: None,
            issuer_secret: None,
            config,
        })
    }
}

impl fmt::Display for Mesh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.encode_bytes();
        let encoded = base58check_encode(&bytes);
        write!(f, "{PREFIX}{SEPARATOR}{encoded}")
    }
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
        .context("Invalid Base58 mesh encoding")?;
    if decoded.len() < 4 {
        bail!("Mesh identifier too short");
    }
    let (payload, received_checksum) = decoded.split_at(decoded.len() - 4);
    let expected_checksum = checksum(payload);
    if received_checksum != expected_checksum {
        bail!("Invalid mesh checksum");
    }
    Ok(payload.to_vec())
}

impl FromStr for Mesh {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let rest = s
            .strip_prefix(PREFIX)
            .with_context(|| format!("Invalid mesh prefix: expected '{PREFIX}'"))?;
        let payload = rest.strip_prefix(SEPARATOR).unwrap_or(rest);
        let bytes = base58check_decode(payload)?;
        Self::decode_bytes(&bytes)
    }
}

#[cfg(test)]
mod mesh_tests {
    use super::{LookupOpts, RelayChoice, SEED_LEN, Mesh, MeshConfig, MeshName};

    fn dummy_seed() -> [u8; SEED_LEN] {
        [7u8; SEED_LEN]
    }

    fn dummy_name() -> MeshName {
        MeshName::new("test").unwrap()
    }

    fn custom_config() -> MeshConfig {
        MeshConfig {
            lookups: LookupOpts {
                mdns: true,
                dht: false,
                relay: RelayChoice::Custom(vec![
                    "https://a.example".parse().unwrap(),
                    "https://b.example".parse().unwrap(),
                ]),
            },
            password: None,
            issuer_pubkey: None,
        }
    }

    #[test]
    fn round_trip_loopback() {
        let mesh = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::loopback());
        let encoded = mesh.to_string();
        assert!(encoded.starts_with("💬"));
        let decoded: Mesh = encoded.parse().unwrap();
        assert_eq!(decoded.seed(), mesh.seed());
        assert_eq!(decoded.name, mesh.name);
        assert_eq!(decoded.config, mesh.config);
        assert!(decoded.is_loopback());
    }

    #[test]
    fn round_trip_public_preset() {
        let mesh = Mesh::new(
            [0xABu8; SEED_LEN],
            dummy_name(),
            MeshConfig::public_preset(),
        );
        let decoded: Mesh = mesh.to_string().parse().unwrap();
        assert_eq!(decoded.seed(), &[0xABu8; SEED_LEN]);
        assert_eq!(decoded.config, mesh.config);
        assert!(!decoded.is_loopback());
        assert_eq!(decoded.network_label(), "public");
    }

    #[test]
    fn round_trip_custom_relay_ladder() {
        let mesh = Mesh::new(dummy_seed(), dummy_name(), custom_config());
        let decoded: Mesh = mesh.to_string().parse().unwrap();
        assert_eq!(decoded.config, custom_config());
    }

    #[test]
    fn seed_drives_rendezvous_identity() {
        let mesh = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::public_preset());
        let decoded: Mesh = mesh.to_string().parse().unwrap();
        assert_eq!(decoded.rendezvous_id(), mesh.rendezvous_id());
        assert_eq!(decoded.rendezvous_ports(), mesh.rendezvous_ports());
    }

    #[test]
    fn different_seeds_yield_different_ids() {
        let one = Mesh::new([1u8; SEED_LEN], dummy_name(), MeshConfig::loopback()).to_string();
        let two = Mesh::new([2u8; SEED_LEN], dummy_name(), MeshConfig::loopback()).to_string();
        assert_ne!(one, two);
    }

    #[test]
    fn different_config_yields_different_id() {
        let one = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::loopback()).to_string();
        let two = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::public_preset()).to_string();
        assert_ne!(one, two, "config is part of the id");
    }

    #[test]
    fn password_verifier_yields_different_id_and_round_trips() {
        let passworded = MeshConfig {
            lookups: LookupOpts::public_preset(),
            password: Some([0x5Au8; 16]),
            issuer_pubkey: None,
        };
        let one = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::public_preset());
        let two = Mesh::new(dummy_seed(), dummy_name(), passworded.clone());
        assert_ne!(one.to_string(), two.to_string());
        let decoded: Mesh = two.to_string().parse().unwrap();
        assert_eq!(decoded.config, passworded);
    }

    #[test]
    fn set_password_bakes_verifier_and_apply_verifies_it() {
        use crate::protocol::crypto::Password;
        let password = Password::new("hunter2".to_owned());

        let mut creator = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::public_preset());
        let passwordless_topic = creator.topic_id();
        let passwordless_rendezvous = creator.rendezvous_id();
        creator.set_password(&password);
        assert!(creator.requires_password());

        // Every derivation switched onto the stretched key.
        assert_ne!(creator.topic_id(), passwordless_topic);
        assert_ne!(creator.rendezvous_id(), passwordless_rendezvous);

        // A joiner decoding the id verifies and lands on identical derivations.
        let mut joiner: Mesh = creator.to_string().parse().unwrap();
        assert!(joiner.requires_password());
        joiner.apply_password(&password).unwrap();
        assert_eq!(joiner.topic_id(), creator.topic_id());
        assert_eq!(joiner.rendezvous_id(), creator.rendezvous_id());
        assert_eq!(joiner.rendezvous_ports(), creator.rendezvous_ports());

        // Wrong password fails locally, against the verifier.
        let mut wrong: Mesh = creator.to_string().parse().unwrap();
        let error = wrong
            .apply_password(&Password::new("hunter3".to_owned()))
            .unwrap_err();
        assert!(error.to_string().contains("wrong password"), "got: {error}");
    }

    #[test]
    fn apply_password_on_passwordless_id_is_rejected() {
        use crate::protocol::crypto::Password;
        let mut mesh = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::public_preset());
        let error = mesh
            .apply_password(&Password::new("hunter2".to_owned()))
            .unwrap_err();
        assert!(error.to_string().contains("no password"), "got: {error}");
    }

    #[test]
    #[should_panic(expected = "passworded mesh derived before the password was applied")]
    fn passworded_mesh_refuses_to_derive_without_the_password() {
        let mut mesh = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::public_preset());
        mesh.config.password = Some([1u8; 16]);
        let _ = mesh.topic_id();
    }

    #[test]
    fn golden_passwordless_id_and_topic_are_pinned() {
        // Byte-for-byte back-compat pin: a passwordless id and its topic
        // must never change encoding (live meshes depend on it). If this
        // fails, the wire format regressed — do not update the constants
        // without a deliberate, versioned format change.
        let mesh = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::public_preset());
        assert_eq!(
            mesh.to_string(),
            "💬://2UXAThUkdBAbiJNXvCt4YeMGQ9myFg7gJJZSr3pG3MAGzUwWmmV7D2NgrWBn1"
        );
        let topic =
            super::crypto::derive_topic_id(mesh.seed(), &mesh.name, &mesh.config_bytes());
        assert_eq!(
            format!("{topic:?}"),
            "TopicId(8c401ccceab4524e1ffaf13bc989172c97e81197bcd0e8c2a2ce8411fb4dddd1)"
        );
    }

    #[test]
    fn from_topic_is_deterministic() {
        let one = Mesh::from_topic("agent-habilis", MeshConfig::public_preset());
        let two = Mesh::from_topic("agent-habilis", MeshConfig::public_preset());
        assert_eq!(one.to_string(), two.to_string());
    }

    #[test]
    fn from_topic_name_is_the_sanitized_string() {
        let mesh = Mesh::from_topic(
            "https://github.com/agent-habilis/agent-square",
            MeshConfig::public_preset(),
        );
        // Scheme stripped, `/`s kept; the 37-char URL exceeds the 32-char cap,
        // so the tail truncates to `…`.
        assert_eq!(mesh.name.as_str(), "github.com/agent-habilis/agent…");
    }

    #[test]
    fn from_topic_differs_by_string() {
        let one = Mesh::from_topic("alpha", MeshConfig::public_preset());
        let two = Mesh::from_topic("beta", MeshConfig::public_preset());
        assert_ne!(one.to_string(), two.to_string());
    }

    #[test]
    fn from_topic_round_trips_through_from_str() {
        let mesh = Mesh::from_topic("agent-habilis", MeshConfig::public_preset());
        let decoded: Mesh = mesh.to_string().parse().expect("decode failed");
        assert_eq!(decoded.seed(), mesh.seed());
        assert_eq!(decoded.name, mesh.name);
        assert_eq!(decoded.config, mesh.config);
    }

    #[test]
    fn invalid_prefix_rejected() {
        let encoded = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::loopback()).to_string();
        let bad = format!("xxx{}", &encoded[super::PREFIX.len()..]);
        assert!(bad.parse::<Mesh>().is_err());
    }

    #[test]
    fn non_bee_prefix_rejected() {
        let encoded = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::loopback()).to_string();
        for bad_prefix in ["sw1", "xyz", "ahs"] {
            let bad = format!("{}{}", bad_prefix, &encoded[super::PREFIX.len()..]);
            assert!(
                bad.parse::<Mesh>().is_err(),
                "expected reject for prefix {bad_prefix}",
            );
        }
    }

    #[test]
    fn invalid_checksum_rejected() {
        let encoded = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::loopback()).to_string();
        let mut bad = encoded.clone();
        let last_index = bad.len() - 1;
        let replacement = if bad.ends_with('1') { "2" } else { "1" };
        bad.replace_range(last_index.., replacement);
        assert!(bad.parse::<Mesh>().is_err());
    }

    #[test]
    fn truncated_bytes_rejected() {
        assert!(Mesh::decode_bytes(&[0u8; 10]).is_err());
    }

    #[test]
    fn unknown_version_rejected() {
        let mesh = Mesh::new(dummy_seed(), dummy_name(), MeshConfig::loopback());
        let mut bytes = mesh.encode_bytes();
        bytes[0] = 2; // an unknown version byte
        assert!(Mesh::decode_bytes(&bytes).is_err());
    }

    #[test]
    fn mesh_id_round_trips_unicode_name() {
        let name = MeshName::new("café-日本-💬").unwrap();
        let mesh = Mesh::new(dummy_seed(), name.clone(), MeshConfig::loopback());
        let decoded: Mesh = mesh.to_string().parse().expect("decode failed");
        assert_eq!(decoded.name, name);
    }

    #[test]
    fn mesh_id_round_trips_max_byte_name() {
        // 32 four-byte scalars = 128 bytes = the most the 1-byte name
        // length field can carry; exercises the encode/decode upper edge.
        let name = MeshName::new("💬".repeat(32)).unwrap();
        assert_eq!(name.as_bytes().len(), 128);
        let mesh = Mesh::new(dummy_seed(), name.clone(), MeshConfig::public_preset());
        let decoded: Mesh = mesh.to_string().parse().expect("decode failed");
        assert_eq!(decoded.name, name);
    }

    mod prop {
        use proptest::{
            array::{uniform16, uniform32},
            prelude::any,
            prop_assert, prop_assert_eq, prop_assert_ne, prop_assume, proptest,
            strategy::Strategy,
        };

        use super::{SEED_LEN, Mesh, MeshConfig, MeshName};

        fn arb_seed() -> impl Strategy<Value = [u8; SEED_LEN]> {
            uniform32(0u8..)
        }

        fn arb_name() -> impl Strategy<Value = MeshName> {
            "[a-z][a-z0-9_-]{0,31}".prop_map(|raw| MeshName::new(raw).unwrap())
        }

        fn arb_config() -> impl Strategy<Value = MeshConfig> {
            (any::<bool>(), proptest::option::of(uniform16(0u8..))).prop_map(
                |(public, verifier)| {
                    let mut config = if public {
                        MeshConfig::public_preset()
                    } else {
                        MeshConfig::loopback()
                    };
                    config.password = verifier;
                    config
                },
            )
        }

        proptest! {
            #![proptest_config(crate::proptest_support::config())]
            #[test]
            fn prop_round_trip(
                seed in arb_seed(),
                name in arb_name(),
                config in arb_config(),
            ) {
                let mesh = Mesh::new(seed, name.clone(), config.clone());
                let decoded: Mesh = mesh.to_string().parse().expect("decode failed");
                prop_assert_eq!(decoded.seed(), mesh.seed());
                prop_assert_eq!(decoded.name, mesh.name);
                prop_assert_eq!(decoded.config, config);
            }

            #[test]
            fn prop_prefix(seed in arb_seed(), name in arb_name()) {
                let mesh = Mesh::new(seed, name, MeshConfig::loopback());
                prop_assert!(mesh.to_string().starts_with("💬"));
            }

            #[test]
            fn prop_deterministic(
                seed in arb_seed(),
                name in arb_name(),
                config in arb_config(),
            ) {
                let mesh = Mesh::new(seed, name, config);
                prop_assert_eq!(mesh.to_string(), mesh.to_string());
            }

            #[test]
            fn prop_distinct_seeds_distinct_ids(
                seed_a in arb_seed(),
                seed_b in arb_seed(),
                name in arb_name(),
            ) {
                prop_assume!(seed_a != seed_b);
                let one = Mesh::new(seed_a, name.clone(), MeshConfig::loopback()).to_string();
                let two = Mesh::new(seed_b, name, MeshConfig::loopback()).to_string();
                prop_assert_ne!(one, two);
            }
        }
    }
}
