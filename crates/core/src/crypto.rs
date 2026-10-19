//! Low-level cryptographic building blocks.
//!
//! Everything here is a thin, versioned wrapper around established primitives:
//!
//! * XChaCha20-Poly1305 (AEAD, 192-bit random nonces) for all symmetric encryption.
//! * HPKE (RFC 9180) base mode with DHKEM(X25519, HKDF-SHA256) / HKDF-SHA256 /
//!   ChaCha20-Poly1305 for encrypting keys to a recipient public key.
//! * Ed25519 (strict verification) for signatures.
//! * HKDF-SHA256 for key derivation and BLAKE3 (derive-key mode) for fingerprints.
//!
//! No custom primitives are defined. The only things we define are byte layouts and
//! the domain-separation strings that bind every ciphertext to its context.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hpke::{Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable};
use sha2::Sha256;

pub type Key32 = [u8; 32];

type HpkeKem = hpke::kem::X25519HkdfSha256;
type HpkeKdf = hpke::kdf::HkdfSha256;
type HpkeAead = hpke::aead::ChaCha20Poly1305;

/// Version byte of the symmetric envelope format.
pub const SYM_VERSION: u8 = 1;
/// Version byte of the HPKE sealed-box format (identifies the HPKE suite).
pub const HPKE_VERSION: u8 = 1;
/// Symmetric envelope header: version (1) + epoch (4, big endian) + nonce (24).
pub const SYM_HEADER_LEN: usize = 1 + 4 + 24;
/// Poly1305 tag length.
pub const TAG_LEN: usize = 16;
/// Per-ciphertext overhead of the symmetric envelope.
pub const SYM_OVERHEAD: usize = SYM_HEADER_LEN + TAG_LEN;

const HKDF_SALT: &[u8] = b"coolpsonotes/v1/hkdf";
const AAD_PREFIX: &[u8] = b"coolpsonotes/v1/aead";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("decryption failed: ciphertext is corrupt, was tampered with, or the key is wrong")]
    Decrypt,
    #[error("malformed ciphertext")]
    Malformed,
    #[error("unsupported format version {0}")]
    Version(u8),
    #[error("invalid key material")]
    InvalidKey,
    #[error("invalid signature")]
    BadSignature,
    #[error("invalid recovery key")]
    BadRecoveryKey,
    #[error("no key available for epoch {0}")]
    MissingEpoch(u32),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// What a symmetric ciphertext is used for. The purpose selects a derived subkey
/// and is bound into the associated data, so a ciphertext produced for one purpose
/// can never be accepted for another (e.g. a server cannot replay a snapshot as an
/// update, or move vault metadata into a document).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Update = 1,
    Snapshot = 2,
    VaultMeta = 3,
    Awareness = 4,
    Attachment = 5,
    KeyChain = 6,
    Prefs = 7,
    LocalCache = 8,
    Recovery = 9,
    /// A note's document key, wrapped under the vault key for vault members.
    DocKey = 10,
    /// Metadata (title) of an individually shared note.
    ObjectMeta = 11,
}

impl Purpose {
    fn label(self) -> &'static [u8] {
        match self {
            Purpose::Update => b"coolpsonotes/v1/key/update",
            Purpose::Snapshot => b"coolpsonotes/v1/key/snapshot",
            Purpose::VaultMeta => b"coolpsonotes/v1/key/vault-meta",
            Purpose::Awareness => b"coolpsonotes/v1/key/awareness",
            Purpose::Attachment => b"coolpsonotes/v1/key/attachment",
            Purpose::KeyChain => b"coolpsonotes/v1/key/key-chain",
            Purpose::Prefs => b"coolpsonotes/v1/key/prefs",
            Purpose::LocalCache => b"coolpsonotes/v1/key/local-cache",
            Purpose::Recovery => b"coolpsonotes/v1/key/recovery",
            Purpose::DocKey => b"coolpsonotes/v1/key/doc-key",
            Purpose::ObjectMeta => b"coolpsonotes/v1/key/object-meta",
        }
    }
}

/// Fill a fixed-size array from the operating system CSPRNG
/// (`crypto.getRandomValues` in browsers).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("operating system random number generator failed");
    b
}

/// HKDF-SHA256 expansion of a 256-bit root key into a 256-bit subkey.
pub fn derive_key(root: &Key32, label: &[u8]) -> Key32 {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), root);
    let mut out = [0u8; 32];
    hk.expand(label, &mut out).expect("32 bytes is a valid HKDF-SHA256 output length");
    out
}

fn push_len_prefixed(buf: &mut Vec<u8>, part: &[u8]) {
    buf.extend_from_slice(&(part.len() as u32).to_be_bytes());
    buf.extend_from_slice(part);
}

fn build_aad(purpose: Purpose, header: &[u8], context: &[&[u8]]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(64);
    aad.extend_from_slice(AAD_PREFIX);
    aad.push(purpose as u8);
    aad.extend_from_slice(header);
    for part in context {
        push_len_prefixed(&mut aad, part);
    }
    aad
}

/// Encrypt `plaintext` with a subkey of `root` derived for `purpose`.
///
/// Layout: `version(1) || epoch(4, BE) || nonce(24) || ciphertext || tag(16)`.
/// The header and every `context` element (e.g. vault id, object id) are
/// authenticated as associated data.
pub fn seal_sym(root: &Key32, epoch: u32, purpose: Purpose, context: &[&[u8]], plaintext: &[u8]) -> Vec<u8> {
    let key = derive_key(root, purpose.label());
    let cipher = XChaCha20Poly1305::new(&key.into());
    let nonce: [u8; 24] = random_bytes();
    let mut header = Vec::with_capacity(SYM_HEADER_LEN);
    header.push(SYM_VERSION);
    header.extend_from_slice(&epoch.to_be_bytes());
    header.extend_from_slice(&nonce);
    let aad = build_aad(purpose, &header, context);
    let ct = cipher
        .encrypt(
            &XNonce::from(nonce),
            chacha20poly1305::aead::Payload { msg: plaintext, aad: &aad },
        )
        .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory buffers");
    let mut out = header;
    out.extend_from_slice(&ct);
    out
}

/// Read the key epoch recorded in a symmetric envelope header without decrypting it.
pub fn sym_epoch(envelope: &[u8]) -> Result<u32> {
    if envelope.len() < SYM_OVERHEAD {
        return Err(CryptoError::Malformed);
    }
    if envelope[0] != SYM_VERSION {
        return Err(CryptoError::Version(envelope[0]));
    }
    Ok(u32::from_be_bytes(envelope[1..5].try_into().unwrap()))
}

/// Decrypt and authenticate a symmetric envelope produced by [`seal_sym`].
pub fn open_sym(root: &Key32, purpose: Purpose, context: &[&[u8]], envelope: &[u8]) -> Result<Vec<u8>> {
    sym_epoch(envelope)?;
    let (header, ct) = envelope.split_at(SYM_HEADER_LEN);
    let nonce: [u8; 24] = header[5..].try_into().unwrap();
    let key = derive_key(root, purpose.label());
    let cipher = XChaCha20Poly1305::new(&key.into());
    let aad = build_aad(purpose, header, context);
    cipher
        .decrypt(&XNonce::from(nonce), chacha20poly1305::aead::Payload { msg: ct, aad: &aad })
        .map_err(|_| CryptoError::Decrypt)
}

/// Generate an X25519 key pair for HPKE. Returns `(secret, public)`.
pub fn x25519_generate() -> (Key32, Key32) {
    let (sk, pk) = HpkeKem::gen_keypair();
    (to_key32(sk.to_bytes().as_slice()), to_key32(pk.to_bytes().as_slice()))
}

/// Compute the X25519 public key for a secret key.
pub fn x25519_public(secret: &Key32) -> Result<Key32> {
    let sk = <HpkeKem as KemTrait>::PrivateKey::from_bytes(secret).map_err(|_| CryptoError::InvalidKey)?;
    Ok(to_key32(HpkeKem::sk_to_pk(&sk).to_bytes().as_slice()))
}

fn to_key32(b: &[u8]) -> Key32 {
    b.try_into().expect("X25519 keys are 32 bytes")
}

/// HPKE base-mode single-shot seal to `recipient_public`.
///
/// Layout: `version(1) || encapsulated_key(32) || ciphertext || tag(16)`.
pub fn hpke_seal(recipient_public: &Key32, info: &[u8], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let pk = <HpkeKem as KemTrait>::PublicKey::from_bytes(recipient_public).map_err(|_| CryptoError::InvalidKey)?;
    let (enc, ct) = hpke::single_shot_seal::<HpkeAead, HpkeKdf, HpkeKem>(&OpModeS::Base, &pk, info, plaintext, aad)
        .map_err(|_| CryptoError::InvalidKey)?;
    let mut out = Vec::with_capacity(1 + 32 + ct.len());
    out.push(HPKE_VERSION);
    out.extend_from_slice(enc.to_bytes().as_slice());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Open an HPKE sealed box produced by [`hpke_seal`].
pub fn hpke_open(recipient_secret: &Key32, info: &[u8], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < 1 + 32 + TAG_LEN {
        return Err(CryptoError::Malformed);
    }
    if sealed[0] != HPKE_VERSION {
        return Err(CryptoError::Version(sealed[0]));
    }
    let sk = <HpkeKem as KemTrait>::PrivateKey::from_bytes(recipient_secret).map_err(|_| CryptoError::InvalidKey)?;
    let enc = <HpkeKem as KemTrait>::EncappedKey::from_bytes(&sealed[1..33]).map_err(|_| CryptoError::Malformed)?;
    hpke::single_shot_open::<HpkeAead, HpkeKdf, HpkeKem>(&OpModeR::Base, &sk, &enc, info, &sealed[33..], aad)
        .map_err(|_| CryptoError::Decrypt)
}

/// Ed25519 public key for a 32-byte seed.
pub fn ed25519_public(seed: &Key32) -> Key32 {
    ed25519_dalek::SigningKey::from_bytes(seed).verifying_key().to_bytes()
}

/// Ed25519 signature over `msg`.
pub fn ed25519_sign(seed: &Key32, msg: &[u8]) -> [u8; 64] {
    use ed25519_dalek::Signer;
    ed25519_dalek::SigningKey::from_bytes(seed).sign(msg).to_bytes()
}

/// Strict Ed25519 verification (rejects malleable and small-order encodings).
pub fn ed25519_verify(public: &Key32, msg: &[u8], sig: &[u8]) -> Result<()> {
    let vk = ed25519_dalek::VerifyingKey::from_bytes(public).map_err(|_| CryptoError::InvalidKey)?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| CryptoError::BadSignature)?;
    vk.verify_strict(msg, &ed25519_dalek::Signature::from_bytes(&sig))
        .map_err(|_| CryptoError::BadSignature)
}

/// Build a length-prefixed, domain-separated byte string. Used for every message
/// that gets signed or hashed so that fields can never be confused.
pub fn statement(domain: &str, parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    push_len_prefixed(&mut out, domain.as_bytes());
    for p in parts {
        push_len_prefixed(&mut out, p);
    }
    out
}

/// BLAKE3 derive-key-mode hash, used for fingerprints and verification codes.
pub fn kdf_hash(context: &str, material: &[u8]) -> [u8; 32] {
    blake3::derive_key(context, material)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sym_roundtrip_and_binding() {
        let key: Key32 = random_bytes();
        let ct = seal_sym(&key, 7, Purpose::Update, &[b"vault", b"obj"], b"hello");
        assert_eq!(sym_epoch(&ct).unwrap(), 7);
        assert_eq!(open_sym(&key, Purpose::Update, &[b"vault", b"obj"], &ct).unwrap(), b"hello");
        // Wrong purpose, context or key fail.
        assert_eq!(open_sym(&key, Purpose::Snapshot, &[b"vault", b"obj"], &ct), Err(CryptoError::Decrypt));
        assert_eq!(open_sym(&key, Purpose::Update, &[b"vault", b"other"], &ct), Err(CryptoError::Decrypt));
        // Context boundaries are unambiguous.
        assert_eq!(open_sym(&key, Purpose::Update, &[b"vaul", b"tobj"], &ct), Err(CryptoError::Decrypt));
        let other: Key32 = random_bytes();
        assert_eq!(open_sym(&other, Purpose::Update, &[b"vault", b"obj"], &ct), Err(CryptoError::Decrypt));
        // Tampering with the epoch in the header is detected.
        let mut tampered = ct.clone();
        tampered[4] ^= 1;
        assert_eq!(open_sym(&key, Purpose::Update, &[b"vault", b"obj"], &tampered), Err(CryptoError::Decrypt));
        let mut tampered = ct;
        let last = tampered.len() - 1;
        tampered[last] ^= 0x80;
        assert_eq!(open_sym(&key, Purpose::Update, &[b"vault", b"obj"], &tampered), Err(CryptoError::Decrypt));
    }

    #[test]
    fn hpke_roundtrip() {
        let (sk, pk) = x25519_generate();
        assert_eq!(x25519_public(&sk).unwrap(), pk);
        let sealed = hpke_seal(&pk, b"info", b"aad", b"secret").unwrap();
        assert_eq!(hpke_open(&sk, b"info", b"aad", &sealed).unwrap(), b"secret");
        assert!(hpke_open(&sk, b"info", b"other", &sealed).is_err());
        assert!(hpke_open(&sk, b"other", b"aad", &sealed).is_err());
        let (sk2, _) = x25519_generate();
        assert!(hpke_open(&sk2, b"info", b"aad", &sealed).is_err());
    }

    #[test]
    fn signatures() {
        let seed: Key32 = random_bytes();
        let pk = ed25519_public(&seed);
        let sig = ed25519_sign(&seed, b"msg");
        ed25519_verify(&pk, b"msg", &sig).unwrap();
        assert!(ed25519_verify(&pk, b"msh", &sig).is_err());
    }
}
