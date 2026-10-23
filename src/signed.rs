//! Signed IDs: a record's ID followed by the first 128 bits of an
//! HMAC-SHA256 over it, as one 64-hex-digit token. Only the ID is stored, so
//! the database holds nothing that opens anything, and a guessed or altered
//! token doesn't verify. Each use signs with its own key.

use hmac::{Hmac, KeyInit, Mac};
use secrecy::{ExposeSecret, SecretBox};
use sha2::Sha256;
use uuid::Uuid;

use crate::engine::files::MasterKey;

/// Signature length in bytes: 128 bits.
const SIGNATURE_LEN: usize = 16;

/// Signs and checks IDs for one purpose.
pub struct Signer {
    key: SecretBox<[u8; 32]>,
}

impl Signer {
    /// A signer whose key comes from `master` and is used only for `purpose`.
    pub fn new(master: &MasterKey, purpose: &str) -> Self {
        Self {
            key: master.derive(purpose),
        }
    }

    /// `id` and its signature, as 64 hex digits.
    pub fn token(&self, id: Uuid) -> String {
        let signature: [u8; 32] = self.mac(id).finalize().into_bytes().into();
        let hex: String = signature[..SIGNATURE_LEN].iter().map(|b| format!("{b:02x}")).collect();
        format!("{}{hex}", id.simple())
    }

    /// The ID in `token`, if its signature is right (compared in constant time).
    pub fn verify(&self, token: &str) -> Option<Uuid> {
        let bytes = from_hex(token)?;
        let (id, signature) = bytes.split_at_checked(16)?;
        if signature.len() != SIGNATURE_LEN {
            return None;
        }
        let id = Uuid::from_slice(id).ok()?;
        self.mac(id).verify_truncated_left(signature).ok()?;
        Some(id)
    }

    fn mac(&self, id: Uuid) -> Hmac<Sha256> {
        // HMAC pads a short key with zeros to its 64-byte block.
        let mut padded = [0_u8; 64];
        padded[..32].copy_from_slice(self.key.expose_secret());
        <Hmac<Sha256> as KeyInit>::new(&padded.into()).chain_update(id.as_bytes())
    }
}

/// Decodes lowercase or uppercase hex; `None` if it isn't hex.
fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}
