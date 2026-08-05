//! Field-level encryption of sensitive data at rest (design M12c).
//!
//! Secret-bearing cloud-init fields (password, user-data, SSH keys) are sealed
//! with AES-256-GCM before they hit PostgreSQL and opened again at the store
//! boundary, so they live encrypted at rest but reach the agent (over mTLS) in
//! plaintext to render the seed ISO.
//!
//! A sealed value is a self-describing string:
//!
//! ```text
//! ENC:1:<key_id>:<base64(nonce)>:<base64(ciphertext||tag)>
//! ```
//!
//! The `key_id` lets a keyring hold several keys — encrypt with the active one,
//! decrypt with whichever key sealed a given value — so keys can be rotated
//! (new writes re-seal under the active key). A per-field *purpose* is mixed in
//! as GCM associated data, binding a ciphertext to its field so a password blob
//! can't be moved into `user_data`. Absent a configured key the platform stores
//! plaintext (backward compatible); opening a non-`ENC:` value passes it
//! through unchanged, which also makes the startup migration idempotent.

use std::collections::HashMap;

use aes_gcm::aead::{Aead, KeyInit, OsRng, Payload};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use vquasar_model::CloudInitSpec;

use crate::config::EncryptionConfig;

const PREFIX: &str = "ENC";
const VERSION: &str = "1";

/// Reason a seal/open failed.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("encryption config invalid: {0}")]
    Config(String),
    #[error("decryption failed (wrong key or tampered data)")]
    Decrypt,
    #[error("sealed value is malformed")]
    Malformed,
    #[error("no key with id {0} is configured")]
    UnknownKey(String),
}

/// A keyring of AES-256-GCM ciphers, keyed by id, with a designated active key.
#[derive(Clone)]
pub struct Cryptor {
    keys: HashMap<String, Aes256Gcm>,
    active_id: String,
}

impl Cryptor {
    /// Build a keyring from config. Returns `Ok(None)` when encryption is not
    /// configured (plaintext mode).
    pub fn from_config(cfg: &EncryptionConfig) -> Result<Option<Self>, CryptoError> {
        let Some(active_material) = cfg.key.as_deref().filter(|k| !k.is_empty()) else {
            return Ok(None);
        };
        let active_id = cfg
            .key_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".to_string());

        let mut keys = HashMap::new();
        keys.insert(active_id.clone(), build_cipher(active_material)?);

        // Decrypt-only keys retired from active use, "id:base64,id2:base64".
        if let Some(old) = &cfg.old_keys {
            for entry in old.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let (id, material) = entry
                    .split_once(':')
                    .ok_or_else(|| CryptoError::Config(format!("bad old_keys entry: {entry}")))?;
                keys.insert(id.to_string(), build_cipher(material)?);
            }
        }
        Ok(Some(Self { keys, active_id }))
    }

    /// Seal plaintext under the active key, binding it to `purpose` (GCM AAD).
    fn seal(&self, purpose: &str, plaintext: &str) -> Result<String, CryptoError> {
        let cipher = self
            .keys
            .get(&self.active_id)
            .ok_or_else(|| CryptoError::UnknownKey(self.active_id.clone()))?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: purpose.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        Ok(format!(
            "{PREFIX}:{VERSION}:{}:{}:{}",
            self.active_id,
            B64.encode(nonce),
            B64.encode(ct)
        ))
    }

    /// Open a sealed value; pass through anything not in the `ENC:` envelope.
    fn open(&self, purpose: &str, value: &str) -> Result<String, CryptoError> {
        if !is_sealed(value) {
            return Ok(value.to_string());
        }
        let parts: Vec<&str> = value.splitn(5, ':').collect();
        // ["ENC", "1", key_id, nonce_b64, ct_b64]
        if parts.len() != 5 || parts[1] != VERSION {
            return Err(CryptoError::Malformed);
        }
        let cipher = self
            .keys
            .get(parts[2])
            .ok_or_else(|| CryptoError::UnknownKey(parts[2].to_string()))?;
        let nonce_bytes = B64.decode(parts[3]).map_err(|_| CryptoError::Malformed)?;
        let ct = B64.decode(parts[4]).map_err(|_| CryptoError::Malformed)?;
        if nonce_bytes.len() != 12 {
            return Err(CryptoError::Malformed);
        }
        let nonce = Nonce::from_slice(&nonce_bytes);
        let pt = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &ct,
                    aad: purpose.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Decrypt)?;
        String::from_utf8(pt).map_err(|_| CryptoError::Malformed)
    }

    /// Seal the sensitive fields of a cloud-init spec in place (idempotent:
    /// already-sealed values are left as-is).
    pub fn seal_cloud_init(&self, ci: &mut CloudInitSpec) -> Result<(), CryptoError> {
        if let Some(pw) = &ci.password {
            if !is_sealed(pw) {
                ci.password = Some(self.seal(PURPOSE_PASSWORD, pw)?);
            }
        }
        if let Some(ud) = &ci.user_data {
            if !is_sealed(ud) {
                ci.user_data = Some(self.seal(PURPOSE_USER_DATA, ud)?);
            }
        }
        for k in ci.ssh_authorized_keys.iter_mut() {
            if !is_sealed(k) {
                *k = self.seal(PURPOSE_SSH_KEY, k)?;
            }
        }
        Ok(())
    }

    /// Open the sensitive fields of a cloud-init spec in place.
    pub fn open_cloud_init(&self, ci: &mut CloudInitSpec) -> Result<(), CryptoError> {
        if let Some(pw) = &ci.password {
            ci.password = Some(self.open(PURPOSE_PASSWORD, pw)?);
        }
        if let Some(ud) = &ci.user_data {
            ci.user_data = Some(self.open(PURPOSE_USER_DATA, ud)?);
        }
        for k in ci.ssh_authorized_keys.iter_mut() {
            *k = self.open(PURPOSE_SSH_KEY, k)?;
        }
        Ok(())
    }
}

const PURPOSE_PASSWORD: &str = "cloud-init.password";
const PURPOSE_USER_DATA: &str = "cloud-init.user_data";
const PURPOSE_SSH_KEY: &str = "cloud-init.ssh_authorized_key";

/// Whether a stored value is in the sealed envelope.
pub fn is_sealed(value: &str) -> bool {
    value.starts_with(&format!("{PREFIX}:{VERSION}:"))
}

fn build_cipher(material_b64: &str) -> Result<Aes256Gcm, CryptoError> {
    let bytes = B64
        .decode(material_b64.trim())
        .map_err(|e| CryptoError::Config(format!("key is not valid base64: {e}")))?;
    if bytes.len() != 32 {
        return Err(CryptoError::Config(format!(
            "key must be 32 bytes (256-bit); got {}",
            bytes.len()
        )));
    }
    let key = Key::<Aes256Gcm>::from_slice(&bytes);
    Ok(Aes256Gcm::new(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cryptor() -> Cryptor {
        // 32 zero bytes, base64.
        let key = B64.encode([0u8; 32]);
        Cryptor::from_config(&EncryptionConfig {
            key: Some(key),
            key_id: Some("k1".into()),
            old_keys: None,
        })
        .unwrap()
        .unwrap()
    }

    #[test]
    fn disabled_when_no_key() {
        let none = Cryptor::from_config(&EncryptionConfig::default()).unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn roundtrip() {
        let c = cryptor();
        let sealed = c.seal(PURPOSE_PASSWORD, "hunter2").unwrap();
        assert!(is_sealed(&sealed));
        assert!(!sealed.contains("hunter2"));
        assert_eq!(c.open(PURPOSE_PASSWORD, &sealed).unwrap(), "hunter2");
    }

    #[test]
    fn open_passes_through_plaintext() {
        let c = cryptor();
        assert_eq!(
            c.open(PURPOSE_PASSWORD, "not-encrypted").unwrap(),
            "not-encrypted"
        );
    }

    #[test]
    fn wrong_purpose_is_rejected() {
        let c = cryptor();
        let sealed = c.seal(PURPOSE_PASSWORD, "secret").unwrap();
        // Opening under a different field's purpose must fail (AAD mismatch).
        assert!(matches!(
            c.open(PURPOSE_USER_DATA, &sealed),
            Err(CryptoError::Decrypt)
        ));
    }

    #[test]
    fn tamper_is_detected() {
        let c = cryptor();
        let mut sealed = c.seal(PURPOSE_PASSWORD, "secret").unwrap();
        // Flip the last base64 char of the ciphertext.
        let last = sealed.pop().unwrap();
        sealed.push(if last == 'A' { 'B' } else { 'A' });
        assert!(c.open(PURPOSE_PASSWORD, &sealed).is_err());
    }

    #[test]
    fn nonce_is_random_per_seal() {
        let c = cryptor();
        let a = c.seal(PURPOSE_USER_DATA, "same").unwrap();
        let b = c.seal(PURPOSE_USER_DATA, "same").unwrap();
        assert_ne!(a, b, "each seal must use a fresh nonce");
    }

    #[test]
    fn rotation_decrypts_old_key() {
        // Seal with k1, then build a keyring whose active key is k2 but which
        // still holds k1 as an old key — it must open the k1 value.
        let k1 = B64.encode([1u8; 32]);
        let k2 = B64.encode([2u8; 32]);
        let old = Cryptor::from_config(&EncryptionConfig {
            key: Some(k1.clone()),
            key_id: Some("k1".into()),
            old_keys: None,
        })
        .unwrap()
        .unwrap();
        let sealed = old.seal(PURPOSE_PASSWORD, "secret").unwrap();

        let rotated = Cryptor::from_config(&EncryptionConfig {
            key: Some(k2),
            key_id: Some("k2".into()),
            old_keys: Some(format!("k1:{k1}")),
        })
        .unwrap()
        .unwrap();
        assert_eq!(rotated.open(PURPOSE_PASSWORD, &sealed).unwrap(), "secret");
        // New seals use k2.
        assert!(rotated
            .seal(PURPOSE_PASSWORD, "x")
            .unwrap()
            .contains(":k2:"));
    }

    #[test]
    fn seal_cloud_init_is_idempotent() {
        let c = cryptor();
        let mut ci = CloudInitSpec {
            hostname: None,
            ssh_authorized_keys: vec!["ssh-ed25519 AAAA".into()],
            password: Some("pw".into()),
            user_data: Some("#cloud-config\n".into()),
        };
        c.seal_cloud_init(&mut ci).unwrap();
        let once = ci.clone();
        c.seal_cloud_init(&mut ci).unwrap(); // second pass must not double-seal
        assert_eq!(ci, once);

        c.open_cloud_init(&mut ci).unwrap();
        assert_eq!(ci.password.as_deref(), Some("pw"));
        assert_eq!(ci.user_data.as_deref(), Some("#cloud-config\n"));
        assert_eq!(ci.ssh_authorized_keys, vec!["ssh-ed25519 AAAA".to_string()]);
    }
}
