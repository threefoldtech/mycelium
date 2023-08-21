//! Abstraction over diffie hellman, symmetric encryption, and hashing.

use core::fmt;
use std::{io, net::Ipv6Addr, ops::Deref, path::Path};

use aes_gcm::{
    aead::{Aead, OsRng as CryptOsRng},
    AeadCore, Aes256Gcm, Key, KeyInit,
};
use blake2::{Blake2b, Digest};
use digest::consts::U16;
use rand_core::OsRng;
use serde::Serialize;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};

/// A public key used as part of Diffie Hellman key exchange. It is derived from a [`SecretKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(x25519_dalek::PublicKey);

/// A secret used as part of Diffie Hellman key exchange.
///
/// This type intentionally does not implement or derive [`Debug`] to avoid accidentally leaking
/// secrets in logs.
#[derive(Clone)]
pub struct SecretKey(x25519_dalek::StaticSecret);

/// A statically computed secret from a [`SecretKey`] and a [`PublicKey`].
///
/// This type intentionally does not implement or derive [`Debug`] to avoid accidentally leaking
/// secrets in logs.
#[derive(Clone)]
pub struct SharedSecret([u8; 32]);

/// Type alias for a 16byte output blake2b hasher.
type Blake2b128 = Blake2b<U16>;

impl SecretKey {
    /// Generate a new `StaticSecret` using [`OsRng`] as an entropy source.
    pub fn new() -> Self {
        SecretKey(x25519_dalek::StaticSecret::new(OsRng))
    }

    /// Load a `SecretKey` from a file.
    pub async fn load_file(path: &Path) -> Result<Self, io::Error> {
        let mut file = File::open(path).await?;
        let mut secret_bytes = [0u8; 32];
        file.read_exact(&mut secret_bytes).await?;

        let secret_key = x25519_dalek::StaticSecret::from(secret_bytes);

        Ok(SecretKey(secret_key))
    }

    /// Saves the `SecretKey` to a file.
    ///
    /// The file is assumed to not exist and will be created. If a file is already present, it will
    /// be overwritten with the new content.
    pub async fn save_file(&self, path: &Path) -> Result<(), io::Error> {
        let mut file = File::create(path).await?;
        file.write_all(&self.0.to_bytes()[..]).await?;

        Ok(())
    }

    /// Computes the [`SharedSecret`] from this `SecretKey` and a [`PublicKey`].
    pub fn shared_secret(&self, other: &PublicKey) -> SharedSecret {
        SharedSecret(self.0.diffie_hellman(&other.0).to_bytes())
    }
}

impl PublicKey {
    /// Generates an [`Ipv6Addr`] from a `PublicKey`.
    ///
    /// The generated address is guaranteed to be part of the `200::/7` range.
    pub fn address(&self) -> Ipv6Addr {
        let mut hasher = Blake2b128::default();
        hasher.update(self.as_bytes());
        let mut buf = hasher.finalize();
        buf[0] = 0x02 | buf[0] & 0x01;
        Ipv6Addr::from(<[u8; 16]>::from(buf))
    }

    /// Convert this `PublicKey` to a byte array.
    pub fn to_bytes(self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// View this `PublicKey` as a byte array.
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl SharedSecret {
    /// Encrypt a message using the `SharedSecret` as key.
    ///
    /// Internally, a new random nonce will be generated using the OS's crypto rng generator. This
    /// nonce is appended to the encrypted data.
    pub fn encrypt(&self, data: &[u8]) -> Vec<u8> {
        let key: Key<Aes256Gcm> = self.0.into();
        let nonce = Aes256Gcm::generate_nonce(&mut CryptOsRng);

        let cipher = Aes256Gcm::new(&key);
        let mut encrypted_data = cipher
            .encrypt(&nonce, data)
            .expect("Encryption can't fail; qed.");

        encrypted_data.extend_from_slice(&nonce);

        encrypted_data
    }

    /// Decrypt a message previously encrytped with an equivalent `SharedSecret`. In other words, a
    /// message that was previously created by the [`SharedSecret::encrypt`] method.
    ///
    /// Internally, this messages assumes that a 12 byte nonce is present at the end of the data.
    /// If the passed in data to decrypt does not contain a valid nonce, decryption fails and an
    /// opaque error is returned. As an extension to this, if the data is not of sufficient length
    /// to contain a valid nonce, an error is returned immediately.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, ()> {
        // Make sure we have sufficient data (i.e. a nonce).
        if data.len() < 12 {
            return Err(());
        }
        let key: Key<Aes256Gcm> = self.0.into();
        let (data, nonce) = data.split_at(data.len() - 12);

        let cipher = Aes256Gcm::new(&key);
        cipher.decrypt(nonce.into(), data).map_err(|_| ())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&faster_hex::hex_string(self.as_bytes()))
    }
}

impl Serialize for PublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&faster_hex::hex_string(self.as_bytes()))
    }
}

impl From<[u8; 32]> for PublicKey {
    /// Given a byte array, construct a `PublicKey`.
    fn from(bytes: [u8; 32]) -> PublicKey {
        PublicKey(x25519_dalek::PublicKey::from(bytes))
    }
}
impl TryFrom<&str> for PublicKey {
    type Error = faster_hex::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let mut output = [0u8; 32];
        faster_hex::hex_decode(value.as_bytes(), &mut output)?;
        Ok(PublicKey::from(output))
    }
}

impl From<&SecretKey> for PublicKey {
    fn from(value: &SecretKey) -> Self {
        PublicKey(x25519_dalek::PublicKey::from(&value.0))
    }
}

impl Deref for SharedSecret {
    type Target = [u8; 32];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
