//! Local protection of key material and caches.
//!
//! A random 256-bit *local key* (`local.key`, mode 0600) is created per
//! profile. The account secret, the device secret and every cached replica are
//! sealed with it (`keys::seal_local`, XChaCha20-Poly1305 bound to a context
//! string).
//!
//! The key file sits in the same profile directory, so this protects against
//! other OS users (file permissions) and stray copies of single files, but
//! **not** against a copy of the whole directory (a backup, a synced folder);
//! moving the key to the OS keyring is planned (TODO.md). Nor does it protect
//! against malware or anyone running as the same OS user. Full-disk encryption is the right
//! defence for a lost laptop; see `docs/desktop-client.md`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use coolpsonotes_core::crypto::random_bytes;
use coolpsonotes_core::keys::{self, AccountSecret, DeviceKey};
use zeroize::Zeroizing;

use crate::fsutil;

const LOCAL_KEY_FILE: &str = "local.key";
const ACCOUNT_FILE: &str = "account.sealed";
const DEVICE_FILE: &str = "device.sealed";
/// A first-device setup not finished yet: keys created but maybe not uploaded,
/// recovery key not shown yet (see `account::login`).
const BOOTSTRAP_FILE: &str = "bootstrap.sealed";

/// The per-profile key protecting local files at rest.
pub struct LocalKey(Zeroizing<[u8; 32]>);

/// Whether `data_dir` holds data sealed with a local key (secrets or vault
/// copies) — which a new key could never open.
fn has_sealed_data(data_dir: &Path) -> bool {
    [ACCOUNT_FILE, DEVICE_FILE, BOOTSTRAP_FILE]
        .iter()
        .any(|f| data_dir.join(f).exists())
        || std::fs::read_dir(data_dir.join("vaults")).is_ok_and(|rd| {
            rd.filter_map(|e| e.ok())
                .any(|e| e.path().join("state.sealed").exists())
        })
}

impl LocalKey {
    pub fn load_or_create(data_dir: &Path) -> Result<LocalKey> {
        fsutil::create_private_dir(data_dir)?;
        let path = data_dir.join(LOCAL_KEY_FILE);
        if let Some(k) = Self::load(&path)? {
            return Ok(k);
        }
        // A fresh key would silently make every sealed file unreadable (and
        // the next save would overwrite them with data under the new key).
        if has_sealed_data(data_dir) {
            bail!(
                "{} is missing, but {} contains encrypted local data (account keys, vault copies) that only \
                 this key can open. Restore local.key from a backup and try again. To start over instead, run \
                 `coolpsonotes logout --forget` for this profile and sign in again (changes that were never \
                 uploaded are lost)",
                path.display(),
                data_dir.display()
            );
        }
        let key: [u8; 32] = random_bytes();
        match fsutil::create_private_file(&path, &key) {
            Ok(()) => Ok(LocalKey(Zeroizing::new(key))),
            // Another process created it first: use theirs.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::load(&path)?.ok_or_else(|| anyhow!("local key disappeared while creating it"))
            }
            Err(e) => Err(e).context("creating the local key file"),
        }
    }

    fn load(path: &Path) -> Result<Option<LocalKey>> {
        let Some(bytes) = fsutil::read_optional(path)? else {
            return Ok(None);
        };
        let bytes = Zeroizing::new(bytes);
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("{} is corrupt (expected 32 bytes)", path.display()))?;
        if fsutil::is_shared_readable(path) {
            tracing::warn!("the local key file is readable by other users; run `chmod 600` on it");
        }
        Ok(Some(LocalKey(Zeroizing::new(key))))
    }

    pub fn seal(&self, context: &str, plaintext: &[u8]) -> Vec<u8> {
        keys::seal_local(&self.0, context, plaintext)
    }

    pub fn open(&self, context: &str, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        keys::open_local(&self.0, context, ciphertext)
            .map(Zeroizing::new)
            .map_err(|e| anyhow!("cannot decrypt local data ({context}): {e}"))
    }

    pub fn write_sealed(&self, path: &Path, context: &str, plaintext: &[u8]) -> Result<()> {
        fsutil::write_atomic(path, &self.seal(context, plaintext), true)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn read_sealed(&self, path: &Path, context: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match fsutil::read_optional(path)? {
            None => Ok(None),
            Some(ct) => self.open(context, &ct).map(Some),
        }
    }
}

/// Sealed account and device secrets of one profile.
pub struct SecretStore {
    dir: PathBuf,
    key: LocalKey,
}

impl SecretStore {
    pub fn open(data_dir: &Path) -> Result<SecretStore> {
        Ok(SecretStore {
            dir: data_dir.to_path_buf(),
            key: LocalKey::load_or_create(data_dir)?,
        })
    }

    pub fn local_key(&self) -> &LocalKey {
        &self.key
    }

    pub fn into_local_key(self) -> LocalKey {
        self.key
    }

    pub fn save_account(&self, user_id: &str, account: &AccountSecret) -> Result<()> {
        let bytes = Zeroizing::new(account.to_bytes());
        self.key.write_sealed(
            &self.dir.join(ACCOUNT_FILE),
            &format!("account/{user_id}"),
            &bytes,
        )
    }

    pub fn load_account(&self, user_id: &str) -> Result<Option<AccountSecret>> {
        let Some(pt) = self
            .key
            .read_sealed(&self.dir.join(ACCOUNT_FILE), &format!("account/{user_id}"))?
        else {
            return Ok(None);
        };
        Ok(Some(AccountSecret::from_bytes(&pt).map_err(|e| {
            anyhow!("stored account secret is invalid: {e}")
        })?))
    }

    pub fn save_device(&self, user_id: &str, device_id: &str, device: &DeviceKey) -> Result<()> {
        self.key.write_sealed(
            &self.dir.join(DEVICE_FILE),
            &format!("device/{user_id}/{device_id}"),
            &device.secret,
        )
    }

    pub fn load_device(&self, user_id: &str, device_id: &str) -> Result<Option<DeviceKey>> {
        let Some(pt) = self.key.read_sealed(
            &self.dir.join(DEVICE_FILE),
            &format!("device/{user_id}/{device_id}"),
        )?
        else {
            return Ok(None);
        };
        let secret: [u8; 32] = pt
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("stored device key is corrupt"))?;
        Ok(Some(DeviceKey::from_secret(secret).map_err(|e| {
            anyhow!("stored device key is invalid: {e}")
        })?))
    }

    /// Remember a first-device setup until it is uploaded and its recovery
    /// key was shown (sealed like the account secret, which it contains the
    /// equivalent of).
    pub fn save_bootstrap(&self, user_id: &str, data: &[u8]) -> Result<()> {
        self.key.write_sealed(
            &self.dir.join(BOOTSTRAP_FILE),
            &format!("bootstrap/{user_id}"),
            data,
        )
    }

    pub fn load_bootstrap(&self, user_id: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        self.key.read_sealed(
            &self.dir.join(BOOTSTRAP_FILE),
            &format!("bootstrap/{user_id}"),
        )
    }

    pub fn forget_bootstrap(&self) -> Result<()> {
        match std::fs::remove_file(self.dir.join(BOOTSTRAP_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Delete the sealed account and device secrets (device was revoked).
    pub fn forget_account(&self) -> Result<()> {
        for f in [ACCOUNT_FILE, DEVICE_FILE] {
            match std::fs::remove_file(self.dir.join(f)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn has_account(&self) -> bool {
        self.dir.join(ACCOUNT_FILE).exists()
    }

    /// Remove every secret and cache of this profile (logout --forget).
    pub fn wipe(data_dir: &Path) -> Result<()> {
        if data_dir.exists() {
            if !data_dir.join(LOCAL_KEY_FILE).exists()
                && !has_sealed_data(data_dir)
                && std::fs::read_dir(data_dir)?.next().is_some()
            {
                bail!(
                    "{} does not look like a coolpsonotes data directory; not deleting it",
                    data_dir.display()
                );
            }
            std::fs::remove_dir_all(data_dir)
                .with_context(|| format!("removing {}", data_dir.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_roundtrip_and_bind_context() {
        let d = tempfile::tempdir().unwrap();
        let s = SecretStore::open(d.path()).unwrap();
        let a = AccountSecret::generate();
        s.save_account("u1", &a).unwrap();
        assert_eq!(s.load_account("u1").unwrap().unwrap().public(), a.public());
        // Bound to the user id: a file copied from another profile is rejected.
        assert!(s.load_account("u2").is_err());
        let dev = DeviceKey::generate();
        s.save_device("u1", "d1", &dev).unwrap();
        assert_eq!(
            s.load_device("u1", "d1").unwrap().unwrap().public,
            dev.public
        );
        // The same local key is reused by later opens.
        let s2 = SecretStore::open(d.path()).unwrap();
        assert!(s2.load_account("u1").unwrap().is_some());
        // Nothing on disk contains the raw secret.
        let raw = std::fs::read(d.path().join(ACCOUNT_FILE)).unwrap();
        assert!(!raw.windows(32).any(|w| w == a.enc_secret));
    }

    #[test]
    fn a_lost_local_key_is_not_silently_replaced() {
        let d = tempfile::tempdir().unwrap();
        let s = SecretStore::open(d.path()).unwrap();
        s.save_account("u1", &AccountSecret::generate()).unwrap();
        drop(s);
        let key = std::fs::read(d.path().join(LOCAL_KEY_FILE)).unwrap();
        std::fs::remove_file(d.path().join(LOCAL_KEY_FILE)).unwrap();
        let e = SecretStore::open(d.path()).err().expect("refused");
        assert!(format!("{e:#}").contains("local.key is missing"), "{e:#}");
        assert!(!d.path().join(LOCAL_KEY_FILE).exists());
        // Vault copies alone count too.
        std::fs::remove_file(d.path().join(ACCOUNT_FILE)).unwrap();
        std::fs::create_dir_all(d.path().join("vaults/v1")).unwrap();
        std::fs::write(d.path().join("vaults/v1/state.sealed"), b"x").unwrap();
        assert!(LocalKey::load_or_create(d.path()).is_err());
        // Restoring the key works again; `logout --forget` can still wipe.
        std::fs::write(d.path().join(LOCAL_KEY_FILE), &key).unwrap();
        assert!(LocalKey::load_or_create(d.path()).is_ok());
        std::fs::remove_file(d.path().join(LOCAL_KEY_FILE)).unwrap();
        SecretStore::wipe(d.path()).unwrap();
        assert!(!d.path().exists());
        // A brand-new profile still gets a key.
        let fresh = tempfile::tempdir().unwrap();
        assert!(LocalKey::load_or_create(fresh.path()).is_ok());
    }
}
