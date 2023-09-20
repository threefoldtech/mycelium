//! Abstraction over diffie hellman, symmetric encryption, and hashing.

use core::fmt;
use std::{error::Error, fmt::Display, io, net::Ipv6Addr, ops::Deref, path::Path};

use aes_gcm::{aead::OsRng, AeadCore, AeadInPlace, Aes256Gcm, Key, KeyInit};
use blake2::{Blake2b, Digest};
use digest::consts::U16;
use serde::Serialize;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};

/// Default MTU for a packet. Ideally this would not be needed and the [`PacketBuffer`] takes a
/// const generic argument which is then expanded with the needed extra space for the buffer,
/// however as it stands const generics can only be used standalone and not in a constant
/// expression. This _is_ possible on nightly rust, with a feature gate (generic_const_exprs).
const PACKET_SIZE: usize = 1400;

/// Size of an AES_GCM tag in bytes.
const AES_TAG_SIZE: usize = 16;

/// Size of an AES_GCM nonce in bytes.
const AES_NONCE_SIZE: usize = 12;

/// Size of a `PacketBuffer`.
const PACKET_BUFFER_SIZE: usize = PACKET_SIZE + AES_TAG_SIZE + AES_NONCE_SIZE;

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

/// A buffer for packets. This holds enough space to  encrypt a packet in place without
/// reallocating.
pub struct PacketBuffer {
    buf: Vec<u8>,
    /// Amount of byts written in the buffer
    size: usize,
}

/// Opaque type indicating decryption failed.
#[derive(Debug, Clone, Copy)]
pub struct DecryptionError;

impl Display for DecryptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Decryption failed, invalid or insufficient encrypted content for this key")
    }
}

impl Error for DecryptionError {}

/// Type alias for a 16byte output blake2b hasher.
type Blake2b128 = Blake2b<U16>;

impl SecretKey {
    /// Generate a new `StaticSecret` using [`OsRng`] as an entropy source.
    pub fn new() -> Self {
        SecretKey(x25519_dalek::StaticSecret::random_from_rng(OsRng))
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

impl Default for SecretKey {
    fn default() -> Self {
        Self::new()
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
    /// Encrypt a [`PacketBuffer`] using the `SharedSecret` as key.
    ///
    /// Internally, a new random nonce will be generated using the OS's crypto rng generator. This
    /// nonce is appended to the encrypted data.
    pub fn encrypt(&self, mut data: PacketBuffer) -> Vec<u8> {
        let key: Key<Aes256Gcm> = self.0.into();
        let nonce = Aes256Gcm::generate_nonce(OsRng);

        let cipher = Aes256Gcm::new(&key);
        let tag = cipher
            .encrypt_in_place_detached(&nonce, &[], &mut data.buf[..data.size])
            .expect("Encryption can't fail; qed.");

        data.buf[data.size..data.size + AES_TAG_SIZE].clone_from_slice(tag.as_slice());
        data.buf[data.size + AES_TAG_SIZE..data.size + AES_TAG_SIZE + AES_NONCE_SIZE]
            .clone_from_slice(&nonce);

        data.buf.truncate(data.size + AES_NONCE_SIZE + AES_TAG_SIZE);

        data.buf
    }

    /// Decrypt a message previously encrytped with an equivalent `SharedSecret`. In other words, a
    /// message that was previously created by the [`SharedSecret::encrypt`] method.
    ///
    /// Internally, this messages assumes that a 12 byte nonce is present at the end of the data.
    /// If the passed in data to decrypt does not contain a valid nonce, decryption fails and an
    /// opaque error is returned. As an extension to this, if the data is not of sufficient length
    /// to contain a valid nonce, an error is returned immediately.
    pub fn decrypt(&self, mut data: Vec<u8>) -> Result<Vec<u8>, DecryptionError> {
        // Make sure we have sufficient data (i.e. a nonce).
        if data.len() < AES_NONCE_SIZE + AES_TAG_SIZE {
            return Err(DecryptionError);
        }

        let data_len = data.len();

        let key: Key<Aes256Gcm> = self.0.into();
        {
            let (data, nonce) = data.split_at_mut(data_len - AES_NONCE_SIZE);
            let (data, tag) = data.split_at_mut(data.len() - AES_TAG_SIZE);

            let cipher = Aes256Gcm::new(&key);
            cipher
                .decrypt_in_place_detached((&*nonce).into(), &[], data, (&*tag).into())
                .map_err(|_| DecryptionError)?;
        }

        // Set the proper size
        data.truncate(data_len - AES_TAG_SIZE - AES_NONCE_SIZE);

        Ok(data)
    }
}

impl PacketBuffer {
    /// Create a new `PacketBuffer`.
    pub fn new() -> Self {
        Self {
            buf: vec![0; PACKET_BUFFER_SIZE],
            size: 0,
        }
    }

    /// Get a reference to the entire useable internal buffer.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        let buf_size = self.buf.len() - AES_TAG_SIZE - AES_NONCE_SIZE;
        &mut self.buf[..buf_size]
    }

    /// Sets the amount of bytes in use by the buffer.
    pub fn set_size(&mut self, size: usize) {
        self.size = size;
    }
}

impl Default for PacketBuffer {
    fn default() -> Self {
        Self::new()
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

impl Deref for PacketBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.buf[..self.size]
    }
}

#[cfg(test)]
mod tests {
    use super::{PacketBuffer, SecretKey, AES_NONCE_SIZE, AES_TAG_SIZE};

    #[test]
    /// Test if encryption works in general. We just create some random value and encrypt it.
    /// Specifically, this will help to catch runtime panics in case AES_TAG_SIZE or AES_NONCE_SIZE
    /// don't have a proper value aligned with the underlying AES_GCM implementation.
    fn encryption_succeeds() {
        let k1 = SecretKey::new();
        let k2 = SecretKey::new();

        let ss = k1.shared_secret(&(&k2).into());

        let mut pb = PacketBuffer::new();
        let data = b"vnno30nv f654q364 vfsv 44"; // Random keyboard smash.

        pb.buffer_mut()[..data.len()].copy_from_slice(data);
        pb.set_size(data.len());

        // We only care that this does not panic.
        let res = ss.encrypt(pb);
        // At the same time, check expected size.
        assert_eq!(res.len(), data.len() + AES_TAG_SIZE + AES_NONCE_SIZE);
    }

    #[test]
    /// Encrypt a value and then decrypt it. This makes sure the decrypt flow and encrypt flow
    /// match, and both follow the expected format. Also, we don't reuse the shared secret for
    /// decryption, but instead generate the secret again the other way round, to simulate a remote
    /// node.
    fn encrypt_decrypt_roundtrip() {
        let k1 = SecretKey::new();
        let k2 = SecretKey::new();

        let ss1 = k1.shared_secret(&(&k2).into());
        let ss2 = k2.shared_secret(&(&k1).into());

        // This assertion is not strictly necessary as it will be checked below implicitly.
        assert_eq!(ss1.as_slice(), ss2.as_slice());

        let data = b"dsafjiqjo23  u2953u8 3oid fjo321j";
        let mut pb = PacketBuffer::new();

        pb.buffer_mut()[..data.len()].copy_from_slice(data);
        pb.set_size(data.len());

        let res = ss1.encrypt(pb);

        let original = ss2.decrypt(res).expect("Decryption works");

        assert_eq!(&original, &data[..]);
    }
}
