//! The snapshot's envelope (D202): what a connector stores is never the
//! export, only this.
//!
//! XChaCha20-Poly1305 under a 32-byte key that Argon2id derives from the
//! store's sync passphrase and a salt of this blob's own. Every seal draws a
//! fresh salt and nonce from the OS, so the same export sealed twice is two
//! different blobs — which is why `sync` compares opened documents, never
//! ciphertext.
//!
//! Format, version 1 (integers little-endian):
//!
//! | bytes  | field                                      |
//! |--------|--------------------------------------------|
//! | 0..8   | magic `TASQXENC`                           |
//! | 8      | format version, `1`                        |
//! | 9..13  | Argon2id memory, KiB (`m_cost`)            |
//! | 13..17 | Argon2id passes (`t_cost`)                 |
//! | 17..21 | Argon2id lanes (`p_cost`)                  |
//! | 21..37 | salt, 16 bytes                             |
//! | 37..61 | XChaCha20 nonce, 24 bytes                  |
//! | 61..   | ciphertext, then the 16-byte Poly1305 tag  |
//!
//! Argon2id is version 0x13 with a 32-byte output. The whole 61-byte header is
//! the AEAD's associated data, so a flipped bit anywhere in the blob fails
//! the tag. The format is libsodium's `crypto_pwhash` (Argon2id13, one lane)
//! and `crypto_aead_xchacha20poly1305_ietf`, so a snapshot can be opened by
//! hand without tasqx.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"TASQXENC";
const VERSION: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const HEADER_LEN: usize = 8 + 1 + 12 + SALT_LEN + NONCE_LEN;
const TAG_LEN: usize = 16;

/// Argon2id's cost. Carried in each blob's header, so a later tasqx can raise
/// it and still open what this one sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Kdf {
    /// Memory, KiB.
    pub(crate) m_cost: u32,
    /// Passes.
    pub(crate) t_cost: u32,
    /// Lanes.
    pub(crate) p_cost: u32,
}

/// What `sync` seals with: RFC 9106's second recommended setting (64 MiB,
/// three passes), on one lane so libsodium can reproduce it.
pub(crate) const DEFAULT_KDF: Kdf = Kdf {
    m_cost: 64 * 1024,
    t_cost: 3,
    p_cost: 1,
};

/// The most a header may ask for before it is refused unopened: a blob from
/// the remote is untrusted, and its header decides how much memory and time
/// opening it costs.
const MAX_KDF: Kdf = Kdf {
    m_cost: 1024 * 1024,
    t_cost: 16,
    p_cost: 16,
};

/// The passphrase and the cost to seal with.
pub(crate) struct SyncKey {
    passphrase: Zeroizing<String>,
    kdf: Kdf,
}

impl SyncKey {
    pub(crate) fn new(passphrase: String) -> SyncKey {
        SyncKey {
            passphrase: Zeroizing::new(passphrase),
            kdf: DEFAULT_KDF,
        }
    }

    /// Decrypt a blob sealed under this passphrase, whatever cost it records.
    pub(crate) fn open(&self, blob: &[u8]) -> Result<Vec<u8>, OpenError> {
        open(&self.passphrase, blob)
    }

    /// The cheapest setting Argon2 allows, so unit tests do not spend 64 MiB
    /// and seconds of a debug build on every seal.
    #[cfg(test)]
    pub(crate) fn cheap(passphrase: &str) -> SyncKey {
        SyncKey {
            passphrase: Zeroizing::new(passphrase.to_string()),
            kdf: Kdf {
                m_cost: 8,
                t_cost: 1,
                p_cost: 1,
            },
        }
    }
}

/// Why a blob would not open.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OpenError {
    /// No tasqx header: plaintext, or something else entirely.
    NotSealed,
    /// A format version this tasqx does not know.
    UnknownVersion(u8),
    /// Shorter than a header and a tag.
    Truncated,
    /// A header asking for more memory or time than tasqx will spend.
    KdfOutOfBounds(Kdf),
    /// The tag did not verify: the wrong passphrase, or bytes changed.
    Unauthentic,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotSealed => f.write_str(
                "it is not encrypted (no tasqx encryption header; plaintext is never merged)",
            ),
            OpenError::UnknownVersion(v) => write!(
                f,
                "it is encrypted in format version {v}, which this tasqx does not know; \
                 upgrade tasqx"
            ),
            OpenError::Truncated => f.write_str("it is truncated, shorter than its own header"),
            OpenError::KdfOutOfBounds(k) => write!(
                f,
                "its header asks for {} KiB of memory, {} passes and {} lanes to open, more \
                 than tasqx will spend",
                k.m_cost, k.t_cost, k.p_cost
            ),
            OpenError::Unauthentic => f.write_str(
                "it cannot be decrypted: this store's sync passphrase is not the one it was \
                 encrypted with, or the snapshot was truncated or altered",
            ),
        }
    }
}

fn derive(passphrase: &str, salt: &[u8], kdf: Kdf) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|e| format!("Argon2 parameters: {e}"))?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|e| format!("Argon2: {e}"))?;
    Ok(key)
}

/// Encrypt `plaintext` into a version-1 blob.
pub(crate) fn seal(key: &SyncKey, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|e| format!("no randomness from the OS: {e}"))?;
    getrandom::fill(&mut nonce).map_err(|e| format!("no randomness from the OS: {e}"))?;
    let mut blob = Vec::with_capacity(HEADER_LEN + plaintext.len() + TAG_LEN);
    blob.extend_from_slice(MAGIC);
    blob.push(VERSION);
    for n in [key.kdf.m_cost, key.kdf.t_cost, key.kdf.p_cost] {
        blob.extend_from_slice(&n.to_le_bytes());
    }
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce);
    let derived = derive(&key.passphrase, &salt, key.kdf)?;
    let cipher = XChaCha20Poly1305::new(&Key::from(*derived));
    let sealed = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plaintext,
                aad: &blob,
            },
        )
        .map_err(|_| "encryption failed".to_string())?;
    blob.extend_from_slice(&sealed);
    Ok(blob)
}

/// Decrypt a blob `seal` made, or say why not. Nothing is returned unless
/// the tag verified, so a caller never sees a byte of a forged document.
pub(crate) fn open(passphrase: &str, blob: &[u8]) -> Result<Vec<u8>, OpenError> {
    if !blob.starts_with(MAGIC) {
        return Err(OpenError::NotSealed);
    }
    match blob.get(MAGIC.len()) {
        None => return Err(OpenError::Truncated),
        Some(&VERSION) => {}
        Some(&v) => return Err(OpenError::UnknownVersion(v)),
    }
    if blob.len() < HEADER_LEN + TAG_LEN {
        return Err(OpenError::Truncated);
    }
    let (header, body) = blob.split_at(HEADER_LEN);
    let u32_at = |i: usize| u32::from_le_bytes(header[i..i + 4].try_into().expect("4 bytes"));
    let kdf = Kdf {
        m_cost: u32_at(9),
        t_cost: u32_at(13),
        p_cost: u32_at(17),
    };
    if kdf.m_cost > MAX_KDF.m_cost || kdf.t_cost > MAX_KDF.t_cost || kdf.p_cost > MAX_KDF.p_cost {
        return Err(OpenError::KdfOutOfBounds(kdf));
    }
    let salt = &header[21..21 + SALT_LEN];
    let nonce: [u8; NONCE_LEN] = header[37..HEADER_LEN].try_into().expect("24 bytes");
    // Parameters Argon2 itself rejects (m_cost under 8 per lane, zero
    // passes) cannot have come from `seal`: an altered header.
    let derived = derive(passphrase, salt, kdf).map_err(|_| OpenError::Unauthentic)?;
    XChaCha20Poly1305::new(&Key::from(*derived))
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: body,
                aad: header,
            },
        )
        .map_err(|_| OpenError::Unauthentic)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &[u8] = br#"{"tasks":[{"title":"Renew the passport"}]}"#;

    fn sealed() -> Vec<u8> {
        seal(&SyncKey::cheap("correct horse battery staple"), DOC).unwrap()
    }

    #[test]
    fn a_sealed_document_opens_with_its_passphrase() {
        let blob = sealed();
        assert!(blob.starts_with(MAGIC));
        assert_eq!(blob.len(), HEADER_LEN + DOC.len() + TAG_LEN);
        assert_eq!(open("correct horse battery staple", &blob).unwrap(), DOC);
    }

    #[test]
    fn the_blob_carries_no_plaintext_and_differs_every_time() {
        let (a, b) = (sealed(), sealed());
        assert_ne!(a, b, "fresh salt and nonce each seal");
        assert!(!a.windows(8).any(|w| w == b"passport"));
    }

    #[test]
    fn the_wrong_passphrase_is_refused() {
        assert_eq!(
            open("Correct horse battery staple", &sealed()),
            Err(OpenError::Unauthentic)
        );
    }

    #[test]
    fn a_flipped_byte_anywhere_after_the_version_is_refused() {
        let blob = sealed();
        for i in MAGIC.len() + 1..blob.len() {
            let mut bad = blob.clone();
            bad[i] ^= 0x01;
            let got = open("correct horse battery staple", &bad);
            assert!(got.is_err(), "byte {i} flipped and it still opened");
        }
    }

    #[test]
    fn a_truncated_blob_is_refused() {
        let blob = sealed();
        for len in [
            0,
            3,
            8,
            9,
            HEADER_LEN,
            HEADER_LEN + TAG_LEN - 1,
            blob.len() - 1,
        ] {
            let got = open("correct horse battery staple", &blob[..len]);
            assert!(got.is_err(), "{len} bytes opened");
        }
        assert_eq!(
            open("correct horse battery staple", &blob[..HEADER_LEN]),
            Err(OpenError::Truncated)
        );
    }

    #[test]
    fn plaintext_is_refused_as_not_sealed() {
        assert_eq!(open("x", DOC), Err(OpenError::NotSealed));
        assert!(OpenError::NotSealed.to_string().contains("not encrypted"));
    }

    #[test]
    fn an_unknown_version_and_a_greedy_header_are_refused_unopened() {
        let mut blob = sealed();
        blob[8] = 2;
        assert_eq!(open("x", &blob), Err(OpenError::UnknownVersion(2)));
        let mut blob = sealed();
        blob[9..13].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            open("x", &blob),
            Err(OpenError::KdfOutOfBounds(_))
        ));
    }
}
