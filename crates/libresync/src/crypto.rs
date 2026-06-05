use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand_core::OsRng;
use rand_core::RngCore;

use crate::{Entry, Error, Result};

const ENCRYPTION_VERSION: u8 = 1;
const NONCE_LEN: usize = 24;
const BLOB_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppKey([u8; 32]);

impl AppKey {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Ok(Self(bytes))
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(Error::Crypto("app key must be 32 bytes".to_string()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub fn encrypt_entries(app_key: &AppKey, entries: Vec<Entry>) -> Result<Vec<Entry>> {
    entries
        .into_iter()
        .map(|entry| encrypt_entry(app_key, entry))
        .collect()
}

pub fn decrypt_entries(app_key: &AppKey, entries: Vec<Entry>) -> Result<Vec<Entry>> {
    entries
        .into_iter()
        .map(|entry| decrypt_entry(app_key, entry))
        .collect()
}

pub fn encrypt_entry(app_key: &AppKey, entry: Entry) -> Result<Entry> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(app_key.as_bytes()));
    let nonce = random_nonce();
    let aad = entry_aad(&entry);
    let payload = Payload {
        msg: &entry.value,
        aad: &aad,
    };

    let ciphertext = cipher
        .encrypt(&nonce, payload)
        .map_err(|_| Error::Crypto("failed to encrypt entry".to_string()))?;

    let mut value = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    value.push(ENCRYPTION_VERSION);
    value.extend_from_slice(nonce.as_slice());
    value.extend_from_slice(&ciphertext);

    Ok(Entry { value, ..entry })
}

pub fn decrypt_entry(app_key: &AppKey, entry: Entry) -> Result<Entry> {
    if entry.value.len() < 1 + NONCE_LEN {
        return Err(Error::Crypto("encrypted entry too short".to_string()));
    }

    let version = entry.value[0];
    if version != ENCRYPTION_VERSION {
        return Err(Error::Crypto("unsupported encryption version".to_string()));
    }

    let nonce = XNonce::from_slice(&entry.value[1..1 + NONCE_LEN]);
    let ciphertext = &entry.value[1 + NONCE_LEN..];
    let aad = entry_aad(&entry);
    let payload = Payload {
        msg: ciphertext,
        aad: &aad,
    };

    let plaintext = XChaCha20Poly1305::new(Key::from_slice(app_key.as_bytes()))
        .decrypt(nonce, payload)
        .map_err(|_| Error::Crypto("failed to decrypt entry".to_string()))?;

    Ok(Entry {
        value: plaintext,
        ..entry
    })
}

pub fn encrypt_blob(app_key: &AppKey, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(app_key.as_bytes()));
    let nonce = random_nonce();
    let payload = Payload {
        msg: plaintext,
        aad,
    };
    let ciphertext = cipher
        .encrypt(&nonce, payload)
        .map_err(|_| Error::Crypto("failed to encrypt blob".to_string()))?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(BLOB_VERSION);
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn decrypt_blob(app_key: &AppKey, blob: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 1 + NONCE_LEN {
        return Err(Error::Crypto("encrypted blob too short".to_string()));
    }
    let version = blob[0];
    if version != BLOB_VERSION {
        return Err(Error::Crypto("unsupported blob version".to_string()));
    }
    let nonce = XNonce::from_slice(&blob[1..1 + NONCE_LEN]);
    let ciphertext = &blob[1 + NONCE_LEN..];
    let payload = Payload {
        msg: ciphertext,
        aad,
    };
    let plaintext = XChaCha20Poly1305::new(Key::from_slice(app_key.as_bytes()))
        .decrypt(nonce, payload)
        .map_err(|_| Error::Crypto("failed to decrypt blob".to_string()))?;
    Ok(plaintext)
}

fn random_nonce() -> XNonce {
    let mut bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut bytes);
    XNonce::from_slice(&bytes).to_owned()
}

fn entry_aad(entry: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    write_len_prefixed(&mut out, entry.key.as_bytes());
    out.extend_from_slice(&entry.clock.counter.to_be_bytes());
    write_len_prefixed(&mut out, entry.clock.device_id.as_bytes());
    out
}

fn write_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = bytes.len() as u64;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LamportClock;

    #[test]
    fn encrypt_round_trip() {
        let key = AppKey::generate().expect("key");
        let entry = Entry {
            key: "alpha".to_string(),
            value: b"hello".to_vec(),
            clock: LamportClock {
                counter: 1,
                device_id: "device".to_string(),
            },
        };

        let encrypted = encrypt_entry(&key, entry.clone()).expect("encrypt");
        assert_ne!(encrypted.value, entry.value);

        let decrypted = decrypt_entry(&key, encrypted).expect("decrypt");
        assert_eq!(decrypted.value, entry.value);
    }

    #[test]
    fn blob_round_trip() {
        let key = AppKey::generate().expect("key");
        let data = b"state";
        let blob = encrypt_blob(&key, data, b"state").expect("encrypt");
        let decoded = decrypt_blob(&key, &blob, b"state").expect("decrypt");
        assert_eq!(decoded, data);
    }

    #[test]
    fn blob_rejects_wrong_key() {
        let key = AppKey::generate().expect("key");
        let other_key = AppKey::generate().expect("other key");
        let data = b"state";
        let blob = encrypt_blob(&key, data, b"state").expect("encrypt");
        let result = decrypt_blob(&other_key, &blob, b"state");
        assert!(result.is_err());
    }
}
