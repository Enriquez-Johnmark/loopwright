//! File encryption. Each file is encrypted with its own random key, and that
//! key is kept only wrapped by a key derived from the master key. Both are
//! bound to the file's ID and tenant, so a file can't be opened with
//! another file's key or passed off as another tenant's.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, SecretBox};
use sha2::Sha256;

use super::FileError;
use crate::engine::ids::{FileId, TenantId};

const NONCE_LEN: usize = 24;

/// The one secret that protects every stored file. Losing it loses the
/// files, which is acceptable: they are temporary (`docs/architecture.md`).
pub struct MasterKey(SecretBox<[u8; 32]>);

impl MasterKey {
    /// Reads a key written by [`MasterKey::generate_hex`].
    pub fn from_hex(hex: &str) -> Result<Self, FileError> {
        let digits = hex.trim().as_bytes();
        if digits.len() != 64 {
            return Err(FileError::BadMasterKey);
        }
        let mut key = Box::new([0_u8; 32]);
        for (byte, pair) in key.iter_mut().zip(digits.chunks(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| FileError::BadMasterKey)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| FileError::BadMasterKey)?;
        }
        Ok(Self(SecretBox::new(key)))
    }

    /// A new random key, as 64 hex digits.
    pub fn generate_hex() -> Result<String, FileError> {
        Ok(random::<32>()?.iter().map(|b| format!("{b:02x}")).collect())
    }

    /// A separate key for one `purpose`, so no two uses share a key.
    pub fn derive(&self, purpose: &str) -> SecretBox<[u8; 32]> {
        // HMAC pads a short key with zeros to its 64-byte block.
        let mut padded = [0_u8; 64];
        padded[..32].copy_from_slice(self.0.expose_secret());
        let mac = <Hmac<Sha256> as KeyInit>::new(&padded.into());
        SecretBox::new(Box::new(mac.chain_update(purpose).finalize().into_bytes().into()))
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey(..)")
    }
}

/// A file as stored: its encrypted contents, and its own key wrapped by the
/// master key. Each starts with the nonce it was encrypted under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The encrypted file.
    pub ciphertext: Vec<u8>,
    /// The file's key, encrypted.
    pub wrapped_key: Vec<u8>,
}

/// Encrypts and decrypts files.
pub struct FileCipher {
    wrapping_key: SecretBox<[u8; 32]>,
}

impl FileCipher {
    /// A cipher whose wrapping key comes from `master`.
    pub fn new(master: &MasterKey) -> Self {
        Self {
            wrapping_key: master.derive("engine file-key wrapping v1"),
        }
    }

    /// Encrypts `plain` as file `id` of `tenant`, under a new random key.
    pub fn seal(&self, tenant: TenantId, id: FileId, plain: &[u8]) -> Result<Sealed, FileError> {
        let file_key = SecretBox::new(Box::new(random::<32>()?));
        let aad = binding(tenant, id);
        Ok(Sealed {
            ciphertext: encrypt(file_key.expose_secret(), plain, &aad)?,
            wrapped_key: encrypt(self.wrapping_key.expose_secret(), file_key.expose_secret(), &aad)?,
        })
    }

    /// Decrypts file `id` of `tenant`; an error if anything was changed,
    /// swapped or moved.
    pub fn open(&self, tenant: TenantId, id: FileId, sealed: &Sealed) -> Result<Vec<u8>, FileError> {
        let aad = binding(tenant, id);
        let file_key = decrypt(self.wrapping_key.expose_secret(), &sealed.wrapped_key, &aad)?;
        let file_key: [u8; 32] = file_key.try_into().map_err(|_| FileError::Undecryptable)?;
        decrypt(&file_key, &sealed.ciphertext, &aad)
    }
}

/// What a file's encryption is bound to.
fn binding(tenant: TenantId, id: FileId) -> Vec<u8> {
    [tenant.as_uuid().as_bytes().as_slice(), id.as_uuid().as_bytes()].concat()
}

fn encrypt(key: &[u8; 32], plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, FileError> {
    let nonce = random::<NONCE_LEN>()?;
    let sealed = XChaCha20Poly1305::new(&Key::from(*key))
        .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad })
        .map_err(|_| FileError::Undecryptable)?;
    Ok([nonce.as_slice(), &sealed].concat())
}

fn decrypt(key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, FileError> {
    let (nonce, rest) = sealed.split_at_checked(NONCE_LEN).ok_or(FileError::Undecryptable)?;
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| FileError::Undecryptable)?;
    XChaCha20Poly1305::new(&Key::from(*key))
        .decrypt(&XNonce::from(nonce), Payload { msg: rest, aad })
        .map_err(|_| FileError::Undecryptable)
}

fn random<const N: usize>() -> Result<[u8; N], FileError> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|err| FileError::Random(err.to_string()))?;
    Ok(bytes)
}
