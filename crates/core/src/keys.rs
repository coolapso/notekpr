//! Key hierarchy.
//!
//! ```text
//!   Recovery key (random 256 bit, shown once) ─┐
//!   Device key (X25519, one per device) ───────┼─ wrap ─▶ Account secret
//!                                              │           ├─ X25519 encryption key  ─ receives vault keys
//!                                              │           ├─ Ed25519 signing key   ─ signs own public key,
//!                                              │           │                          device approvals
//!                                              │           └─ symmetric key          ─ private prefs
//!   Account public key ─ HPKE ─▶ Vault key epoch N (random 256 bit, one per vault per rotation)
//!                                   ├─ HKDF subkeys per purpose (updates, snapshots, meta, ...)
//!                                   └─ key chain: epoch N encrypts epoch N-1
//! ```
//!
//! The server only ever stores public keys and ciphertexts of this hierarchy.

use std::collections::BTreeMap;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::crypto::{self, CryptoError, Key32, Purpose, Result};

const ACCOUNT_SECRET_VERSION: u8 = 1;
const INFO_ACCOUNT_TO_DEVICE: &[u8] = b"coolpsonotes/v1/hpke/account-to-device";
const INFO_VAULT_KEY: &[u8] = b"coolpsonotes/v1/hpke/vault-key";

/// A user's (or agent's) long-term secret key material.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct AccountSecret {
    pub enc_secret: Key32,
    pub sign_secret: Key32,
    pub sym: Key32,
}

/// Public half of an account key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPublic {
    pub enc_public: Key32,
    pub sign_public: Key32,
}

impl AccountSecret {
    pub fn generate() -> Self {
        let (enc_secret, _) = crypto::x25519_generate();
        AccountSecret {
            enc_secret,
            sign_secret: crypto::random_bytes(),
            sym: crypto::random_bytes(),
        }
    }

    pub fn public(&self) -> AccountPublic {
        AccountPublic {
            enc_public: crypto::x25519_public(&self.enc_secret).expect("valid X25519 secret"),
            sign_public: crypto::ed25519_public(&self.sign_secret),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(97);
        v.push(ACCOUNT_SECRET_VERSION);
        v.extend_from_slice(&self.enc_secret);
        v.extend_from_slice(&self.sign_secret);
        v.extend_from_slice(&self.sym);
        v
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        if b.len() != 97 {
            return Err(CryptoError::Malformed);
        }
        if b[0] != ACCOUNT_SECRET_VERSION {
            return Err(CryptoError::Version(b[0]));
        }
        Ok(AccountSecret {
            enc_secret: b[1..33].try_into().unwrap(),
            sign_secret: b[33..65].try_into().unwrap(),
            sym: b[65..97].try_into().unwrap(),
        })
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        crypto::ed25519_sign(&self.sign_secret, msg)
    }

    /// Self-signature binding this account's public keys to its user id.
    pub fn sign_account_key(&self, user_id: &str, key_version: u32) -> [u8; 64] {
        self.sign(&account_key_statement(user_id, key_version, &self.public()))
    }

    /// Signature authorising a device to receive this account's secret.
    pub fn sign_device_approval(
        &self,
        user_id: &str,
        device_id: &str,
        device_public: &Key32,
    ) -> [u8; 64] {
        self.sign(&device_approval_statement(
            user_id,
            device_id,
            device_public,
        ))
    }

    /// Signature authorising replacement of the recovery envelope.
    pub fn sign_recovery_update(&self, user_id: &str, envelope: &[u8]) -> [u8; 64] {
        self.sign(&recovery_update_statement(user_id, envelope))
    }

    /// Encrypt private per-user data (preferences, favourites, TOFU pins).
    pub fn seal_private(&self, user_id: &str, plaintext: &[u8]) -> Vec<u8> {
        crypto::seal_sym(
            &self.sym,
            0,
            Purpose::Prefs,
            &[user_id.as_bytes()],
            plaintext,
        )
    }

    pub fn open_private(&self, user_id: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
        crypto::open_sym(&self.sym, Purpose::Prefs, &[user_id.as_bytes()], ciphertext)
    }
}

impl AccountPublic {
    /// Short human-comparable fingerprint (128 bits, hex, grouped) of the public keys.
    /// Users compare these out-of-band to rule out a server substituting keys.
    pub fn fingerprint(&self) -> String {
        let mut m = Vec::with_capacity(64);
        m.extend_from_slice(&self.sign_public);
        m.extend_from_slice(&self.enc_public);
        let h = crypto::kdf_hash("coolpsonotes v1 account fingerprint", &m);
        let hex = data_encoding::HEXUPPER.encode(&h[..16]);
        hex.as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Verify the account's self-signature.
    pub fn verify(&self, user_id: &str, key_version: u32, signature: &[u8]) -> Result<()> {
        crypto::ed25519_verify(
            &self.sign_public,
            &account_key_statement(user_id, key_version, self),
            signature,
        )
    }

    pub fn verify_recovery_update(
        &self,
        user_id: &str,
        envelope: &[u8],
        signature: &[u8],
    ) -> Result<()> {
        crypto::ed25519_verify(
            &self.sign_public,
            &recovery_update_statement(user_id, envelope),
            signature,
        )
    }

    pub fn verify_device_approval(
        &self,
        user_id: &str,
        device_id: &str,
        device_public: &Key32,
        signature: &[u8],
    ) -> Result<()> {
        crypto::ed25519_verify(
            &self.sign_public,
            &device_approval_statement(user_id, device_id, device_public),
            signature,
        )
    }
}

pub fn recovery_update_statement(user_id: &str, envelope: &[u8]) -> Vec<u8> {
    crypto::statement(
        "coolpsonotes/v1/sig/recovery-update",
        &[user_id.as_bytes(), envelope],
    )
}

pub fn account_key_statement(user_id: &str, key_version: u32, public: &AccountPublic) -> Vec<u8> {
    crypto::statement(
        "coolpsonotes/v1/sig/account-key",
        &[
            user_id.as_bytes(),
            &key_version.to_be_bytes(),
            &public.enc_public,
            &public.sign_public,
        ],
    )
}

pub fn device_approval_statement(user_id: &str, device_id: &str, device_public: &Key32) -> Vec<u8> {
    crypto::statement(
        "coolpsonotes/v1/sig/device-approval",
        &[user_id.as_bytes(), device_id.as_bytes(), device_public],
    )
}

/// Per-device X25519 key pair. The secret never leaves the device.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DeviceKey {
    pub secret: Key32,
    #[zeroize(skip)]
    pub public: Key32,
}

impl DeviceKey {
    pub fn generate() -> Self {
        let (secret, public) = crypto::x25519_generate();
        DeviceKey { secret, public }
    }

    pub fn from_secret(secret: Key32) -> Result<Self> {
        let public = crypto::x25519_public(&secret)?;
        Ok(DeviceKey { secret, public })
    }

    pub fn unwrap_account(
        &self,
        sealed: &[u8],
        user_id: &str,
        device_id: &str,
    ) -> Result<AccountSecret> {
        let aad = crypto::statement("aad", &[user_id.as_bytes(), device_id.as_bytes()]);
        let mut pt = crypto::hpke_open(&self.secret, INFO_ACCOUNT_TO_DEVICE, &aad, sealed)?;
        let res = AccountSecret::from_bytes(&pt);
        pt.zeroize();
        res
    }
}

/// Encrypt the account secret to a (verified) device public key.
pub fn wrap_account_for_device(
    account: &AccountSecret,
    device_public: &Key32,
    user_id: &str,
    device_id: &str,
) -> Result<Vec<u8>> {
    let aad = crypto::statement("aad", &[user_id.as_bytes(), device_id.as_bytes()]);
    let mut pt = account.to_bytes();
    let res = crypto::hpke_seal(device_public, INFO_ACCOUNT_TO_DEVICE, &aad, &pt);
    pt.zeroize();
    res
}

/// Code shown on both the new device and the approving device. The user checks
/// they match before approving, which defeats a server that substitutes its own
/// device key. 60 bits makes grinding a colliding key during the approval
/// window impractical.
pub fn device_verification_code(user_id: &str, device_id: &str, device_public: &Key32) -> String {
    let m = crypto::statement(
        "device-code",
        &[user_id.as_bytes(), device_id.as_bytes(), device_public],
    );
    let h = crypto::kdf_hash("coolpsonotes v1 device verification code", &m);
    // 60 bits -> 12 base32 characters.
    let s = data_encoding::BASE32_NOPAD.encode(&h[..8]);
    let s = &s[..12];
    format!("{}-{}-{}", &s[0..4], &s[4..8], &s[8..12])
}

/// High-entropy recovery key. Shown once at setup; wraps the account secret.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RecoveryKey(pub Key32);

impl RecoveryKey {
    pub fn generate() -> Self {
        RecoveryKey(crypto::random_bytes())
    }

    /// Human-friendly encoding: base32 of key || 2-byte checksum, groups of 5.
    pub fn to_display(&self) -> String {
        let mut b = self.0.to_vec();
        b.extend_from_slice(&checksum(&self.0));
        let s = data_encoding::BASE32_NOPAD.encode(&b);
        b.zeroize();
        s.as_bytes()
            .chunks(5)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parse a recovery key, ignoring case, whitespace and dashes and tolerating
    /// common look-alike characters.
    pub fn parse(input: &str) -> Result<Self> {
        let cleaned: String = input
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .map(|c| match c.to_ascii_uppercase() {
                '0' => 'O',
                '1' => 'I',
                '8' => 'B',
                c => c,
            })
            .collect();
        let bytes = data_encoding::BASE32_NOPAD
            .decode(cleaned.as_bytes())
            .map_err(|_| CryptoError::BadRecoveryKey)?;
        if bytes.len() != 34 {
            return Err(CryptoError::BadRecoveryKey);
        }
        let key: Key32 = bytes[..32].try_into().unwrap();
        if checksum(&key) != bytes[32..34] {
            return Err(CryptoError::BadRecoveryKey);
        }
        Ok(RecoveryKey(key))
    }

    pub fn wrap_account(&self, account: &AccountSecret, user_id: &str) -> Vec<u8> {
        let mut pt = account.to_bytes();
        let ct = crypto::seal_sym(&self.0, 0, Purpose::Recovery, &[user_id.as_bytes()], &pt);
        pt.zeroize();
        ct
    }

    pub fn unwrap_account(&self, envelope: &[u8], user_id: &str) -> Result<AccountSecret> {
        let mut pt = crypto::open_sym(&self.0, Purpose::Recovery, &[user_id.as_bytes()], envelope)?;
        let res = AccountSecret::from_bytes(&pt);
        pt.zeroize();
        res
    }
}

fn checksum(key: &Key32) -> [u8; 2] {
    let h = crypto::kdf_hash("coolpsonotes v1 recovery checksum", key);
    [h[0], h[1]]
}

/// Symmetric key of one vault epoch.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct VaultKey(pub Key32);

impl VaultKey {
    pub fn generate() -> Self {
        VaultKey(crypto::random_bytes())
    }
}

fn vault_key_aad(vault_id: &str, epoch: u32, user_id: &str) -> Vec<u8> {
    crypto::statement(
        "aad",
        &[
            vault_id.as_bytes(),
            &epoch.to_be_bytes(),
            user_id.as_bytes(),
        ],
    )
}

/// Encrypt a vault key to a member's account public key.
pub fn wrap_vault_key(
    key: &VaultKey,
    vault_id: &str,
    epoch: u32,
    recipient_user_id: &str,
    recipient_enc_public: &Key32,
) -> Result<Vec<u8>> {
    crypto::hpke_seal(
        recipient_enc_public,
        INFO_VAULT_KEY,
        &vault_key_aad(vault_id, epoch, recipient_user_id),
        &key.0,
    )
}

pub fn unwrap_vault_key(
    account: &AccountSecret,
    sealed: &[u8],
    vault_id: &str,
    epoch: u32,
    user_id: &str,
) -> Result<VaultKey> {
    let mut pt = crypto::hpke_open(
        &account.enc_secret,
        INFO_VAULT_KEY,
        &vault_key_aad(vault_id, epoch, user_id),
        sealed,
    )?;
    let key: Key32 = pt
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed)?;
    pt.zeroize();
    Ok(VaultKey(key))
}

/// All known epochs of one vault's key.
#[derive(Clone)]
pub struct VaultKeyring {
    vault_id: String,
    keys: BTreeMap<u32, VaultKey>,
}

impl VaultKeyring {
    pub fn new(vault_id: impl Into<String>) -> Self {
        VaultKeyring {
            vault_id: vault_id.into(),
            keys: BTreeMap::new(),
        }
    }

    pub fn vault_id(&self) -> &str {
        &self.vault_id
    }

    pub fn insert(&mut self, epoch: u32, key: VaultKey) {
        self.keys.insert(epoch, key);
    }

    pub fn get(&self, epoch: u32) -> Option<&VaultKey> {
        self.keys.get(&epoch)
    }

    pub fn current_epoch(&self) -> Option<u32> {
        self.keys.keys().next_back().copied()
    }

    pub fn current(&self) -> Option<(u32, &VaultKey)> {
        self.keys.iter().next_back().map(|(e, k)| (*e, k))
    }

    pub fn epochs(&self) -> impl Iterator<Item = u32> + '_ {
        self.keys.keys().copied()
    }

    /// Recover older epochs by walking the key chain backwards from the newest key we hold.
    /// `chain` maps epoch N to the encryption of epoch N-1's key under epoch N.
    pub fn fill_from_chain(&mut self, chain: &BTreeMap<u32, Vec<u8>>) -> Result<()> {
        let ctx = self.vault_id.clone();
        self.fill_from_chain_with(chain, &ctx)
    }

    /// Like [`fill_from_chain`](Self::fill_from_chain) with an explicit link
    /// context (document keys use [`object_scope`]).
    pub fn fill_from_chain_with(
        &mut self,
        chain: &BTreeMap<u32, Vec<u8>>,
        context: &str,
    ) -> Result<()> {
        let Some(mut epoch) = self.current_epoch() else {
            return Ok(());
        };
        // Vault epochs start at 1, document key epochs at DOC_EPOCH_BIT | 1.
        let floor = (epoch & DOC_EPOCH_BIT) | 1;
        while epoch > floor {
            if !self.keys.contains_key(&(epoch - 1)) {
                let Some(link) = chain.get(&epoch) else { break };
                let newer = self
                    .keys
                    .get(&epoch)
                    .expect("walking from known key")
                    .clone();
                let older = decrypt_chain_link(&newer, epoch, link, context)?;
                self.keys.insert(epoch - 1, older);
            }
            epoch -= 1;
        }
        Ok(())
    }

    pub fn encrypt(&self, purpose: Purpose, object_id: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        let (epoch, key) = self.current().ok_or(CryptoError::MissingEpoch(0))?;
        Ok(crypto::seal_sym(
            &key.0,
            epoch,
            purpose,
            &[self.vault_id.as_bytes(), object_id.as_bytes()],
            plaintext,
        ))
    }

    pub fn decrypt(&self, purpose: Purpose, object_id: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let epoch = crypto::sym_epoch(ciphertext)?;
        let key = self
            .keys
            .get(&epoch)
            .ok_or(CryptoError::MissingEpoch(epoch))?;
        crypto::open_sym(
            &key.0,
            purpose,
            &[self.vault_id.as_bytes(), object_id.as_bytes()],
            ciphertext,
        )
    }

    /// Vault metadata (name, description) is bound to the vault only.
    pub fn encrypt_meta(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.encrypt(Purpose::VaultMeta, "", plaintext)
    }

    pub fn decrypt_meta(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        self.decrypt(Purpose::VaultMeta, "", ciphertext)
    }
}

/// Epochs of document keys (individually shared notes) have the top bit set,
/// so they can never be confused with vault key epochs. The epoch is part of
/// every ciphertext header and authenticated.
pub const DOC_EPOCH_BIT: u32 = 0x8000_0000;

pub fn is_doc_epoch(epoch: u32) -> bool {
    epoch & DOC_EPOCH_BIT != 0
}

/// First epoch of a document key.
pub const FIRST_DOC_EPOCH: u32 = DOC_EPOCH_BIT | 1;

/// Context used for wrapping and chaining a note's document key: binds the
/// key material to one note of one vault.
pub fn object_scope(vault_id: &str, object_id: &str) -> String {
    format!("{vault_id}/{object_id}")
}

/// Wrap a note's document key under the vault's current key, for vault members.
pub fn wrap_doc_key_for_vault(
    vault_ring: &VaultKeyring,
    object_id: &str,
    epoch: u32,
    doc_key: &VaultKey,
) -> Result<Vec<u8>> {
    let (vault_epoch, vk) = vault_ring.current().ok_or(CryptoError::MissingEpoch(0))?;
    Ok(crypto::seal_sym(
        &vk.0,
        vault_epoch,
        Purpose::DocKey,
        &[
            vault_ring.vault_id().as_bytes(),
            object_id.as_bytes(),
            &epoch.to_be_bytes(),
        ],
        &doc_key.0,
    ))
}

pub fn unwrap_doc_key_for_vault(
    vault_ring: &VaultKeyring,
    object_id: &str,
    epoch: u32,
    envelope: &[u8],
) -> Result<VaultKey> {
    let vault_epoch = crypto::sym_epoch(envelope)?;
    let vk = vault_ring
        .get(vault_epoch)
        .ok_or(CryptoError::MissingEpoch(vault_epoch))?;
    let mut pt = crypto::open_sym(
        &vk.0,
        Purpose::DocKey,
        &[
            vault_ring.vault_id().as_bytes(),
            object_id.as_bytes(),
            &epoch.to_be_bytes(),
        ],
        envelope,
    )?;
    let key: Key32 = pt
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed)?;
    pt.zeroize();
    Ok(VaultKey(key))
}

/// Encrypt epoch `new_epoch - 1`'s key under the key of `new_epoch`.
pub fn encrypt_chain_link(
    newer: &VaultKey,
    new_epoch: u32,
    older: &VaultKey,
    vault_id: &str,
) -> Vec<u8> {
    crypto::seal_sym(
        &newer.0,
        new_epoch,
        Purpose::KeyChain,
        &[vault_id.as_bytes()],
        &older.0,
    )
}

pub fn decrypt_chain_link(
    newer: &VaultKey,
    new_epoch: u32,
    link: &[u8],
    vault_id: &str,
) -> Result<VaultKey> {
    if crypto::sym_epoch(link)? != new_epoch {
        return Err(CryptoError::Malformed);
    }
    let mut pt = crypto::open_sym(&newer.0, Purpose::KeyChain, &[vault_id.as_bytes()], link)?;
    let key: Key32 = pt
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed)?;
    pt.zeroize();
    Ok(VaultKey(key))
}

/// Key protecting a client's local cache at rest.
pub fn seal_local(key: &Key32, context: &str, plaintext: &[u8]) -> Vec<u8> {
    crypto::seal_sym(
        key,
        0,
        Purpose::LocalCache,
        &[context.as_bytes()],
        plaintext,
    )
}

pub fn open_local(key: &Key32, context: &str, ciphertext: &[u8]) -> Result<Vec<u8>> {
    crypto::open_sym(key, Purpose::LocalCache, &[context.as_bytes()], ciphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_roundtrip_signature_fingerprint() {
        let a = AccountSecret::generate();
        let b = AccountSecret::from_bytes(&a.to_bytes()).unwrap();
        assert_eq!(a.public(), b.public());
        let sig = a.sign_account_key("u1", 1);
        a.public().verify("u1", 1, &sig).unwrap();
        assert!(a.public().verify("u2", 1, &sig).is_err());
        assert!(a.public().verify("u1", 2, &sig).is_err());
        assert_eq!(a.public().fingerprint().len(), 39);
    }

    #[test]
    fn device_wrap() {
        let a = AccountSecret::generate();
        let d = DeviceKey::generate();
        let sealed = wrap_account_for_device(&a, &d.public, "u1", "d1").unwrap();
        let back = d.unwrap_account(&sealed, "u1", "d1").unwrap();
        assert_eq!(back.public(), a.public());
        assert!(d.unwrap_account(&sealed, "u1", "d2").is_err());
        let code = device_verification_code("u1", "d1", &d.public);
        assert_eq!(code.len(), 14);
        let sig = a.sign_device_approval("u1", "d1", &d.public);
        a.public()
            .verify_device_approval("u1", "d1", &d.public, &sig)
            .unwrap();
        assert!(
            a.public()
                .verify_device_approval("u1", "d2", &d.public, &sig)
                .is_err()
        );
    }

    #[test]
    fn recovery() {
        let a = AccountSecret::generate();
        let rk = RecoveryKey::generate();
        let shown = rk.to_display();
        let parsed = RecoveryKey::parse(&shown.to_lowercase().replace('-', " ")).unwrap();
        assert_eq!(parsed.0, rk.0);
        let env = rk.wrap_account(&a, "u1");
        assert_eq!(
            parsed.unwrap_account(&env, "u1").unwrap().public(),
            a.public()
        );
        assert!(parsed.unwrap_account(&env, "u2").is_err());
        // A single typo is caught by the checksum.
        let mut chars: Vec<char> = shown.chars().collect();
        chars[3] = if chars[3] == 'A' { 'B' } else { 'A' };
        assert!(RecoveryKey::parse(&chars.into_iter().collect::<String>()).is_err());
    }

    #[test]
    fn document_keys() {
        let mut vault = VaultKeyring::new("v");
        vault.insert(1, VaultKey::generate());
        let d1 = VaultKey::generate();
        let env = wrap_doc_key_for_vault(&vault, "n", FIRST_DOC_EPOCH, &d1).unwrap();
        assert_eq!(
            unwrap_doc_key_for_vault(&vault, "n", FIRST_DOC_EPOCH, &env)
                .unwrap()
                .0,
            d1.0
        );
        assert!(unwrap_doc_key_for_vault(&vault, "other", FIRST_DOC_EPOCH, &env).is_err());
        assert!(unwrap_doc_key_for_vault(&vault, "n", FIRST_DOC_EPOCH + 1, &env).is_err());
        // Chains of document keys stop at the first document epoch.
        let d2 = VaultKey::generate();
        let scope = object_scope("v", "n");
        let mut chain = BTreeMap::new();
        chain.insert(
            FIRST_DOC_EPOCH + 1,
            encrypt_chain_link(&d2, FIRST_DOC_EPOCH + 1, &d1, &scope),
        );
        let mut ring = VaultKeyring::new("v");
        ring.insert(FIRST_DOC_EPOCH + 1, d2);
        ring.fill_from_chain_with(&chain, &scope).unwrap();
        assert_eq!(
            ring.epochs().collect::<Vec<_>>(),
            vec![FIRST_DOC_EPOCH, FIRST_DOC_EPOCH + 1]
        );
        let ct = ring.encrypt(Purpose::Update, "n", b"x").unwrap();
        assert!(is_doc_epoch(crypto::sym_epoch(&ct).unwrap()));
        // The vault key cannot read document-key content.
        assert!(vault.decrypt(Purpose::Update, "n", &ct).is_err());
    }

    #[test]
    fn vault_keys_and_chain() {
        let alice = AccountSecret::generate();
        let k1 = VaultKey::generate();
        let sealed = wrap_vault_key(&k1, "v", 1, "alice", &alice.public().enc_public).unwrap();
        let k1b = unwrap_vault_key(&alice, &sealed, "v", 1, "alice").unwrap();
        assert_eq!(k1.0, k1b.0);
        assert!(unwrap_vault_key(&alice, &sealed, "v", 2, "alice").is_err());
        assert!(unwrap_vault_key(&alice, &sealed, "w", 1, "alice").is_err());

        let mut ring1 = VaultKeyring::new("v");
        ring1.insert(1, k1.clone());
        let old_ct = ring1.encrypt(Purpose::Update, "doc", b"old").unwrap();

        // Rotate twice; a newcomer receiving only epoch 3 can read epoch 1 content.
        let k2 = VaultKey::generate();
        let k3 = VaultKey::generate();
        let mut chain = BTreeMap::new();
        chain.insert(2, encrypt_chain_link(&k2, 2, &k1, "v"));
        chain.insert(3, encrypt_chain_link(&k3, 3, &k2, "v"));
        let mut ring = VaultKeyring::new("v");
        ring.insert(3, k3);
        ring.fill_from_chain(&chain).unwrap();
        assert_eq!(ring.epochs().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(
            ring.decrypt(Purpose::Update, "doc", &old_ct).unwrap(),
            b"old"
        );
        assert!(ring.decrypt(Purpose::Update, "doc2", &old_ct).is_err());
        let new_ct = ring.encrypt(Purpose::Update, "doc", b"new").unwrap();
        assert_eq!(crypto::sym_epoch(&new_ct).unwrap(), 3);
        // The old keyring cannot read post-rotation content.
        assert_eq!(
            ring1.decrypt(Purpose::Update, "doc", &new_ct),
            Err(CryptoError::MissingEpoch(3))
        );
    }
}
